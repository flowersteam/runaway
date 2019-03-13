// orchestra-cli/web/mod.rs
// Author: Alexandre Péré
///
/// Contains two web applications to control orchestra-cli from a gui:
/// + A start application which allows to create, clone, or open a campaign
/// + An application which allows to create jobs and observe results.
///
/// The two applications are built using the `actix-web` crate. It is most recommended to look at
/// actix documentation before proceding with the following.
///
/// The html pages are generated using handlebar templates. Some of the logic of the application
/// happen on the client side. Be sure to look at the templates to understand how all fits.

// MODULES
/// + `app.rs` contains the app
/// + `start.rs` contains the start app
pub mod app;
pub mod start;