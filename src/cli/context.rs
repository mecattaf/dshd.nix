//! Building the `Context`: the invoking user, the platform's service
//! manager and its state root, the tools on `PATH`, the client environment
//! and cwd.

use std::env;

use crate::cli::{Asked, Context};
use crate::error::Error;
use crate::platform;
use crate::sys;
use crate::tool::Tool;

/// See [`Context::from_env`].
pub(super) fn from_env(asked: Asked, json: bool) -> Result<Context, Error> {
    let user = sys::current_user()?;
    let manager = platform::native()?;
    let state_root = manager.state_root().clone();
    let cwd =
        env::current_dir().map_err(|error| Error::io("read", "the working directory", error))?;
    Ok(Context {
        asked,
        json,
        manager,
        state_root,
        nix_store: Tool::named("nix-store"),
        nix: Tool::named("nix"),
        journalctl: Tool::named("journalctl"),
        xdg_open: Tool::named("xdg-open"),
        home: user.home,
        user: user.name,
        environment: env::vars_os().collect(),
        cwd,
    })
}
