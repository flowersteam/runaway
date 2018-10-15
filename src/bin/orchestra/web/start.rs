// orchestra/web/start.rs
// Author: Alexandre Péré
///
/// This module contains a one-page web application that allows the user to select a campaign,
/// before starting the actual application which needs a campaign to work on.
/// Several options are available: 
/// + Clone an existing campaign from a remote repository
/// + Create a new campaign from an experiment repository
/// + Open a local campaign

// IMPORTS
use actix::System;
use handlebars;
use regex;
use actix_web::{http, middleware::Logger, server, App, Form, HttpRequest, HttpResponse, State,};
use std::collections::HashMap;
use std::{fs, path, thread, time};
use futures::Future;
use std::sync::{Arc, Mutex, mpsc};
use liborchestra::{repository, git};

// STATE
/// The state allows to share app-wide data on the application. Check-out the actix doc.
#[derive(Debug)]
pub struct StartState {
    templates: handlebars::Handlebars,
    campaign: Arc<Mutex<Option<repository::Campaign>>>,
}
impl StartState {
    /// Set the campaign
    pub fn set_campaign(&self, campaign: repository::Campaign) {
        if self.campaign.lock().unwrap().is_some() {
            panic!("Campaign was already loaded");
        }
        *self.campaign.lock().unwrap() = Some(campaign);
    }
}

// UI
/// This module contains the handlers that create html pages
mod ui {

    use super::*;

    /// Handle GET "/"
    pub fn index(req: HttpRequest<StartState>) -> HttpResponse {
        let html = {
            let mut data = HashMap::new();
            data.insert("content", "start_content");
            req.state().templates.render("page", &data).unwrap()
        };
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(html)
    }
}

// API
/// This module contains the handlers for api points
mod api {

    use super::*;

    #[derive(Deserialize, Debug)]
    pub struct CloneCampaignRequest {
        remote_url: String,
        local_path: path::PathBuf,
    }

    #[derive(Deserialize, Debug)]
    pub struct NewCampaignRequest {
        experiment_url: String,
        campaign_url: String,
        local_path: path::PathBuf,
    }

    #[derive(Deserialize, Debug)]
    pub struct OpenCampaignRequest {
        local_path: path::PathBuf,
    }

    #[derive(Serialize)]
    pub struct PostResponse {
        success: bool,
        error: String,
    }

    /// Handle POST "/api/clone_campaign"
    pub fn clone_campaign((req, state): (Form<CloneCampaignRequest>, State<StartState>)) -> HttpResponse {
        match repository::Campaign::from_url(req.remote_url.as_str(), &req.local_path) {
            Ok(campaign) => {
                state.set_campaign(campaign);
                HttpResponse::build(http::StatusCode::OK).json(PostResponse {
                    success: true,
                    error: String::from(""),
                })
            }
            Err(error) => {
                HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                    PostResponse {
                        success: false,
                        error: format!("Failed to clone campaign: {}", error),
                    },
                )
            }
        }
    }

    /// Handle POST "/api/new_campaign"
    pub fn new_campaign((req, state): (Form<NewCampaignRequest>, State<StartState>)) -> HttpResponse {
        // We extract repo name
        let reg = regex::Regex::new(r"^.*/([^/]+)\.git$").unwrap();
        let name = &reg.captures(&req.campaign_url).unwrap()[1];
        if let Err(error) = fs::create_dir_all(&req.local_path.join(name)) {
            return HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                PostResponse {
                    success: false,
                    error: format!("Failed to create directory: {}", error),
                },
            );
        }
        if let Err(error) = git::init(&req.local_path.join(name)) {
            return HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                PostResponse {
                    success: false,
                    error: format!("Failed to initialize git repo: {}", error),
                },
            );
        }
        if let Err(error) = git::add_remote("origin", &req.campaign_url, &req.local_path.join(name)){
            return HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                PostResponse {
                    success: false,
                    error: format!("Failed to add origin remote: {}", error),
                },
            );
        }
        let campaign = match repository::Campaign::init(&req.local_path.join(name), &req.experiment_url) {
                Ok(cmp) => cmp,
                Err(error) => {
                    return HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                        PostResponse {
                            success: false,
                            error: format!("Failed to initialize expegit repo: {}", error),
                        },
                    );
                }
        };
        if let Err(error) = git::push(Some("origin"), &campaign.get_path()) {
            return HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                PostResponse {
                    success: false,
                    error: format!("Failed to set-upstream: {}", error),
                },
            );
        }
        state.set_campaign(campaign);
        HttpResponse::build(http::StatusCode::OK).json(PostResponse {
            success: true,
            error: String::from(""),
        })
    }

    /// Handle POST "/api/open_campaign"
    pub fn open_campaign((req, state): (Form<OpenCampaignRequest>, State<StartState>)) -> HttpResponse {
        match repository::Campaign::from_path(&req.local_path) {
            Ok(campaign) => {
                state.set_campaign(campaign);
                HttpResponse::build(http::StatusCode::OK).json(PostResponse {
                    success: true,
                    error: String::from(""),
                })
            }
            Err(error) => {
                HttpResponse::build(http::StatusCode::INTERNAL_SERVER_ERROR).json(
                    PostResponse {
                        success: false,
                        error: format!("Failed to open campaign: {}", error),
                    },
                )
            }
        }
    }    
}

/// Helper function to load the handlebars templates
fn load_handlebar_templates() -> handlebars::Handlebars{
    let mut hdbr = handlebars::Handlebars::new();
    hdbr.register_partial("sidebar", include_str!("../../../../assets/templates/sidebar.hbs")).unwrap();
    hdbr.register_partial("topbar", include_str!("../../../../assets/templates/topbar.hbs")).unwrap();
    hdbr.register_partial("home_content", include_str!("../../../../assets/templates/home_content.hbs"),).unwrap();
    hdbr.register_partial("start_content", include_str!("../../../../assets/templates/start_content.hbs")).unwrap();
    hdbr.register_partial("execution_content", include_str!("../../../../assets/templates/execution_content.hbs"),).unwrap();
    hdbr.register_partial("tasks_overview_content", include_str!("../../../../assets/templates/tasks_overview_content.hbs"),).unwrap();
    hdbr.register_partial("manual_generation_content", include_str!("../../../../assets/templates/manual_generation_content.hbs"),).unwrap();
    hdbr.register_partial("executions_overview_content",include_str!("../../../../assets/templates/executions_overview_content.hbs"),).unwrap();
    hdbr.register_template_string("page", include_str!("../../../../assets/templates/page.hbs")).unwrap();
    hdbr
}

/// launches the start application and wait until the campaign field ot the state was set to some
/// value, which is returned.
pub fn launch_start(port: u32) -> repository::Campaign{
    // We open a channel to get the address of the server back.
    let (tx, rx) = mpsc::channel();
    // We generate
    let campaign = Arc::new(Mutex::new(None));
    let campaign_t = Arc::clone(&campaign);
    // We launch a server inside a thread.
    thread::spawn(move || {
        let system = System::new("start-server");
        let addr = server::new(move || {
            App::with_state(StartState {templates: load_handlebar_templates(), campaign: campaign_t.clone()})
                .middleware(Logger::default())
                .resource("/", |r| r.method(http::Method::GET).with(ui::index))
                .resource("/api/clone_campaign", |r| {
                    r.method(http::Method::POST).with(api::clone_campaign)
                })
                .resource("/api/new_campaign", |r| {
                    r.method(http::Method::POST).with(api::new_campaign)
                })
                .resource("/api/open_campaign", |r| {
                    r.method(http::Method::POST).with(api::open_campaign)
                })
            })
            .bind(format!("127.0.0.1:{}", port).as_str())
            .unwrap()
            .workers(10)
            .start();
        let _ = tx.send(addr);
        let _ = system.run();
        });
    // We retrieve the server address
    let addr = rx.recv().unwrap();
    // We loop waiting for the campaign to be initialized and exit the server
    loop {
        let campaign_opt = campaign.lock().unwrap();
        if let Some(ref cmp) = *campaign_opt {
            let _ = addr.send(server::StopServer {graceful: false}).wait();
            return cmp.clone();
        }
        else if !addr.connected(){
            panic!("Server disconnected");
        }
        else{
            thread::sleep(time::Duration::from_millis(10));
        }
    }
}
