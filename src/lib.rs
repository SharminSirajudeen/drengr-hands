//! Eyes and hands for AI agents on Android and iOS.
//!
//! The binary is the product; this library is what it is built from, and is
//! published so the pieces can be reused. Four things worth knowing before
//! reading further:
//!
//! - [`transport::DeviceTransport`] is the whole device abstraction. Android
//!   (ADB), iOS (simctl plus a bundled Swift runner) and cloud providers
//!   (Appium) each implement it, and nothing else in the crate knows which one
//!   it is talking to. A new backend implements this trait and changes nothing
//!   else.
//! - [`screen`] turns a screenshot and an accessibility tree into numbered,
//!   tappable elements. [`screen::text_scene`] renders that as roughly 300
//!   tokens instead of a 100KB image, which is why an agent can watch a whole
//!   flow without spending its context on pixels.
//! - [`situation`] reports what *changed* between two observations rather than
//!   what is on screen now. That diff is what makes an action's result legible.
//! - [`ooda`] is the optional autonomous loop behind `drengr run`. In MCP mode
//!   it is unused: the calling model decides, and no API key is involved.

pub mod credentials;
pub mod diag;
pub mod driver;
pub mod expect_network;
pub mod explore;
mod guards;
pub mod http;
pub mod key_store;
pub mod mcp;
pub mod network;
pub mod onboard;
pub mod ooda;
pub mod paths;
pub mod redact;
pub mod run_outcome;
pub mod runner;
pub mod screen;
pub mod sdk;
pub mod session;
pub mod situation;
pub mod source_guard;
pub mod transport;
pub mod validate;
