//! URL path router with parameter substitution.

use std::sync::Arc;

use crate::error::{Error, Result};

/// A router that matches URL paths to target templates with parameter substitution.
///
/// Supports path parameters like `{host}` and wildcards like `{*path}`:
/// - `/ssh/{host}` matches `/ssh/myserver` with `host=myserver`
/// - `/files/{*path}` matches `/files/a/b/c` with `path=a/b/c`
///
/// Parameters can be used in target templates:
/// - Pattern `/ssh/{host}` with target `{host}:22` → `/ssh/myserver` resolves to `myserver:22`
#[derive(Clone)]
pub struct Router {
    inner: Arc<matchit::Router<String>>,
    count: usize,
}

impl Router {
    /// Create a new empty router.
    pub fn new() -> Self {
        Router {
            inner: Arc::new(matchit::Router::new()),
            count: 0,
        }
    }

    /// Insert a route with a pattern and target template.
    ///
    /// The pattern supports `{param}` for single segment and `{*param}` for wildcards.
    /// The target template can reference captured parameters.
    pub fn insert(&mut self, pattern: impl Into<String>, target: impl Into<String>) -> Result<()> {
        let pattern = pattern.into();
        let target = target.into();
        let router = Arc::make_mut(&mut self.inner);
        router
            .insert(pattern.clone(), target)
            .map_err(|e| Error::config(format!("invalid route pattern '{}': {}", pattern, e)))?;
        self.count += 1;
        Ok(())
    }

    /// Match a path and return the resolved target string.
    ///
    /// Returns `None` if no route matches.
    pub fn resolve(&self, path: &str) -> Option<String> {
        // Try exact match first
        if let Ok(matched) = self.inner.at(path) {
            return Some(substitute_params(matched.value, matched.params));
        }

        // Try without trailing slash
        let normalized = path.trim_end_matches('/');
        if normalized != path
            && let Ok(matched) = self.inner.at(normalized) {
                return Some(substitute_params(matched.value, matched.params));
            }

        None
    }

    /// Check if the router has any routes.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("count", &self.count)
            .finish()
    }
}

/// Substitute parameters into a target template.
fn substitute_params(template: &str, params: matchit::Params) -> String {
    let mut result = template.to_string();
    for (key, value) in params.iter() {
        let placeholder = format!("{{{}}}", key);
        result = result.replace(&placeholder, value);
        // Also handle wildcard syntax {*key}
        let wildcard_placeholder = format!("{{*{}}}", key);
        result = result.replace(&wildcard_placeholder, value);
    }
    result
}
