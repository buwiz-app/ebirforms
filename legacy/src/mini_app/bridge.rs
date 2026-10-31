//! The mini apps' bridge to the `bir` host module, behind the named
//! `bir_app_*` tools ([`super::v1::app_tools`]).
//!
//! Any exported function is callable by name with JSON arguments, exactly as
//! `app/` calls it through gpui-shell. Synchronous functions run here, on the
//! thread that owns the database. An asynchronous one is started here (its
//! argument checks and database reads happen on this thread) and handed back
//! as [`Call::Pending`] so the transport can drive it elsewhere.

use gpui_shell::{HostArguments, HostModule};
use serde_json::Value;

use super::scope;
use crate::bir_module::{json, to_json};

/// Functions answered before sign-in: the Buwiz account itself, so a mini
/// app can sign in. Everything else waits for the account gate.
const ACCOUNT_PREFIX: &str = "cloud_account_";

/// One host call's outcome.
pub(crate) enum Call {
    Ready(Result<Value, String>),
    /// An async host function, already started. Run it to completion off the
    /// database thread (it holds no thread-local state).
    Pending(Box<dyn FnOnce() -> Result<Value, String> + Send>),
}

#[cfg(test)]
impl Call {
    /// Drive a pending call on this thread.
    pub(crate) fn wait(self) -> Result<Value, String> {
        match self {
            Call::Ready(result) => result,
            Call::Pending(run) => run(),
        }
    }
}

pub(crate) struct Bridge {
    module: HostModule,
}

impl Bridge {
    pub(crate) fn new() -> Self {
        Self {
            module: crate::bir_module::module(),
        }
    }

    /// Call `function` with `args`, held to `scope` (a 9-digit TIN root) when given.
    pub(crate) fn call(&self, function: &str, args: Vec<Value>, scope: Option<&str>) -> Call {
        match self.start(function, args, scope) {
            Ok(call) => call,
            Err(error) => Call::Ready(Err(error)),
        }
    }

    fn start(&self, function: &str, args: Vec<Value>, scope: Option<&str>) -> Result<Call, String> {
        if !self.module.has(function) {
            return Err(format!(
                "unknown bir function `{function}`; see the `bir` host module declarations for the names"
            ));
        }
        if !function.starts_with(ACCOUNT_PREFIX) && !crate::account_gate::is_unlocked() {
            return Err(crate::account_gate::LOCKED_MESSAGE.to_string());
        }
        if let Some(scope) = scope {
            scope::check_args(function, &args, scope)?;
        }
        // The page calls this once device approval finishes. Windowless, it
        // opens the gate and the database (which starts the submission
        // worker) without the updater or the menu bar.
        if function == "cloud_account_session_opened" {
            crate::account_gate::set_unlocked(true);
            crate::bir_module::open_database_at_launch();
            return Ok(Call::Ready(Ok(Value::Null)));
        }
        let arguments = HostArguments::new(args.iter().cloned().map(json));
        let scope = scope.map(str::to_string);
        let function_name = function.to_string();
        let finish = move |value: Value| match &scope {
            Some(scope) => scope::filter_result(&function_name, &args, value, scope),
            None => Ok(value),
        };
        if self.module.is_async(function) {
            let future = self
                .module
                .begin(function, &arguments)
                .map_err(|error| error.message().to_string())?;
            return Ok(Call::Pending(Box::new(move || {
                let value = futures::executor::block_on(future)
                    .map_err(|error| error.message().to_string())?;
                finish(to_json(&value))
            })));
        }
        let value = self
            .module
            .call(function, &arguments)
            .map_err(|error| error.message().to_string())?;
        Ok(Call::Ready(finish(to_json(&value))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT: &str = "123456788";
    const OTHER: &str = "987654321";

    fn bridge() -> Bridge {
        crate::bir_module::use_ephemeral_database();
        crate::account_gate::set_unlocked(true);
        Bridge::new()
    }

    fn create(bridge: &Bridge, root: &str, name: &str) {
        bridge
            .call(
                "create_taxpayer",
                vec![json!({
                    "tin_root": root,
                    "name": name,
                    "taxpayer_type": "Individual",
                    "head_office": {"rdo_code": "040"}
                })],
                None,
            )
            .wait()
            .unwrap_or_else(|error| panic!("create {root}: {error}"));
    }

    #[test]
    fn sync_and_async_functions_dispatch_by_name() {
        let bridge = bridge();
        let catalog = bridge.call("forms_catalog", vec![], None).wait().unwrap();
        assert!(catalog.as_array().is_some_and(|forms| !forms.is_empty()));
        create(&bridge, ROOT, "Scratch One");
        let opened = bridge.call(
            "screen_form_open",
            vec![
                json!(format!("{ROOT}00000")),
                json!("0619E"),
                json!(2026),
                json!(1),
            ],
            None,
        );
        assert!(matches!(opened, Call::Pending(_)));
        let opened = opened.wait().unwrap();
        assert_eq!(opened["form_code"], "0619E");
    }

    #[test]
    fn an_unknown_function_is_named_in_the_error() {
        let error = bridge()
            .call("no_such_fn", vec![], None)
            .wait()
            .unwrap_err();
        assert!(
            error.contains("unknown bir function `no_such_fn`"),
            "{error}"
        );
    }

    #[test]
    fn a_scope_filters_lists_and_refuses_other_roots() {
        let bridge = bridge();
        create(&bridge, ROOT, "Scratch One");
        create(&bridge, OTHER, "Scratch Two");
        let all = bridge.call("list_taxpayers", vec![], None).wait().unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        let scoped = bridge
            .call("list_taxpayers", vec![], Some(ROOT))
            .wait()
            .unwrap();
        let roots: Vec<&str> = scoped
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["tin_root"].as_str())
            .collect();
        assert_eq!(roots, vec![ROOT]);
        let error = bridge
            .call("get_taxpayer", vec![json!(OTHER)], Some(ROOT))
            .wait()
            .unwrap_err();
        assert!(error.contains("outside this app's scope"), "{error}");
        let mine = bridge
            .call("get_taxpayer", vec![json!(ROOT)], Some(ROOT))
            .wait()
            .unwrap();
        assert_eq!(mine["tin_root"], ROOT);
    }

    #[test]
    fn a_locked_gate_allows_only_the_account_functions() {
        let bridge = bridge();
        crate::account_gate::force_locked(|| {
            let error = bridge
                .call("list_taxpayers", vec![], None)
                .wait()
                .unwrap_err();
            assert_eq!(error, crate::account_gate::LOCKED_MESSAGE);
            let error = bridge
                .call("forms_catalog", vec![], None)
                .wait()
                .unwrap_err();
            assert_eq!(error, crate::account_gate::LOCKED_MESSAGE);
            let status = bridge
                .call("cloud_account_status", vec![], None)
                .wait()
                .unwrap();
            assert!(status.get("signed_in").is_some(), "{status}");
        });
    }
}
