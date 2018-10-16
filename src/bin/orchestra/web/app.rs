// orchestra/web/app.rs
// Author: Alexandre Péré
///
/// This module contains a web application that allows the user to act on a campaign. It allows to
/// generate batch of executions and observe the results.

// IMPORTS
use actix_web::{ http, middleware::Logger, server, App, Form, HttpRequest, HttpResponse, State};
use actix_web::fs::StaticFiles;
use std::collections::HashMap;
use std::{ffi, fs, path};
use std::sync::{Arc, Mutex, MutexGuard};
use liborchestra::{repository, tasks, git, misc};
use handlebars;
use serde_json;

// STATE
/// The state of the application. Note: Such a state will be created or every thread of the http
/// server. Since we want the campaign and the taskqueue to be shared between the different threads,
/// those fields are protected by a mutex. At server instantiation, single campaign and taskqueue
/// will be created, and clones will be provided to threads-local states.
#[derive(Debug)]
pub struct ApplicationState {
    campaign: Arc<Mutex<repository::Campaign>>,
    taskqueue: Arc<Mutex<tasks::TaskQueue>>,
    templates: handlebars::Handlebars,
}
impl ApplicationState {
    /// Returns a mutexguard on the campaign
    fn get_campaign(&self) -> MutexGuard<repository::Campaign>{
        self.campaign.lock().unwrap()
    }

    // Returns a mutexguard on the taskqueue
    fn get_taskqueue(&self) -> MutexGuard<tasks::TaskQueue>{
        self.taskqueue.lock().unwrap()
    }
}

// UI
/// This module contains handlers for html pages.
mod ui {
    use super::*;

    /// Handles GET "/home"
    pub fn home(state: State<ApplicationState>) -> HttpResponse {
        let campaign = state.get_campaign();
        let campaign_url = campaign.get_campaign_url();
        let exp_url = campaign.get_experiment_url();
        let execution_count = format!("{}", campaign.get_executions().len());
        let html = {
            let data = json!({
                "content": "home_content",
                "menu_item": "home",
                "campaign_name": campaign.get_campaign_name(),
                "campaign_web_url": &campaign_url,
                "campaign_repo_url": campaign.get_campaign_origin_url(),
                "exp_name": campaign.get_experiment_name(),
                "exp_web_url": &exp_url,
                "exp_repo_url": campaign.get_experiment_origin_url(),
                "execution_count": &execution_count
                });
            state.templates.render("page", &data).unwrap()
        };
        // We return the response
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(html)
    }

    /// Handles GET "/execution/{execution_id}"
    pub fn execution(req: HttpRequest<ApplicationState>) -> HttpResponse {
        // We retrieve data
        let campaign = req.state().get_campaign();
        let execution_id: String = req.match_info().query("execution_id").unwrap();
        let execution_path = campaign.get_executions_path().join(execution_id);
        // Does the execution exists?
        match repository::Execution::from_path(&execution_path) {
            Err(_) => HttpResponse::build(http::StatusCode::NOT_FOUND).finish(),
            Ok(execution) => {
                // We retrieve more data
                let experiment_url = vec![campaign.get_experiment_url().as_str(), "/tree/", execution.get_commit()].join("");
                let remote_url = vec![campaign.get_campaign_url().as_str(), "/tree/master/excs/", execution.get_identifier()].join("");
                let executor =  execution.get_executor().clone().unwrap_or_else(|| String::from("None"));
                let start_date = execution.get_execution_date().clone().unwrap_or_else(|| String::from("None"));
                let duration = execution.get_execution_duration().map_or_else(
                    || String::from("None"),
                    |u| format!("{}:{}:{}", u / 3600, u % 3600 / 60, u % 3600 % 60),
                );
                let exit_code = execution.get_execution_exit_code().map_or_else(
                    || String::from("None"),
                    |u| format!("{}", u)
                );
                let stdout = execution.get_execution_stdout().clone().map_or_else(
                    || String::from("None"),
                    |u| u.to_owned()
                ).replace("\\n", "\n").replace("\n", "<br>");
                let stderr = execution.get_execution_stderr().clone().map_or_else(
                    || String::from("None"),
                    |u| u.to_owned()
                ).replace("\\n", "\n").replace("\n", "<br>");
                let authorized_images_extensions = vec!["gif", "png", "jpg"];
                let execution_path = campaign.get_executions_path();

                fn read_all_files(path: &path::PathBuf) -> Vec<path::PathBuf>{
                   path.read_dir()
                   .expect("failed to read dir")
                   .map(|u| u.unwrap().path())
                   .inspect(|x| println!("after path get {:?}", x))
                   .map(|u| {
                       if u.is_dir(){
                           read_all_files(&u)
                       }
                       else{
                           vec![u]
                       }
                   })
                   .flatten()
                   .collect()
                }
                let image_urls: Vec<String> = read_all_files(&execution.get_data_path())
                    .iter()
                    .filter(|u| u.is_file() && u.extension().is_some())
                    .filter(|u| {
                        authorized_images_extensions
                            .iter()
                            .any(|&x| x == u.extension().unwrap().to_str().unwrap())
                    })
                    .map(|u| u.strip_prefix(execution_path.clone()).unwrap().to_owned())
                    .map(|u| format!("/static/{}", u.to_str().unwrap()))
                    .collect();
                let files: Vec<serde_json::Value> = read_all_files(&execution.get_data_path())
                    .iter()
                    .filter(|u| u.is_file() && u.extension().is_some())
                    .filter(|u| {
                        authorized_images_extensions
                            .iter()
                            .all(|&x| x != u.extension().unwrap().to_str().unwrap())
                    })
                    .map(|u| u.strip_prefix(execution_path.clone()).unwrap().to_owned())
                    .map(|u| {
                        json!({
                            "address": format!("/static/{}", u.to_str().unwrap()), 
                            "name": u.file_stem().unwrap().to_str().unwrap(),
                            "path": u.to_str().unwrap(),
                            })
                    })
                    .collect();
                // We construct the page
                let html = {
                    let data = json!({
                        "content": "execution_content",
                        "menu_item": "executions",
                        "execution_id": execution.get_identifier(),
                        "execution_remote_url": &remote_url,
                        "experiment_commit_url": &experiment_url,
                        "execution_commit": execution.get_commit(),
                        "execution_parameters": execution.get_parameters(),
                        "execution_executor": &executor,
                        "execution_start_time": &start_date,
                        "execution_duration": &duration,
                        "execution_exit_code": &exit_code,
                        "execution_stdout": &stdout,
                        "execution_stderr": &stderr,
                        "figures": &image_urls,
                        "files": &files,
                    });
                    req.state().templates.render("page", &data).unwrap()
                };
                // We return the response
                HttpResponse::build(http::StatusCode::OK)
                    .content_type("text/html")
                    .body(html)
            }
        }
    }

    /// Handles GET "/manual_generation"
    pub fn manual_generation(req: HttpRequest<ApplicationState>) -> HttpResponse {
        // We render the template
        let html = {
            let mut data = HashMap::new();
            data.insert("content", "manual_generation_content");
            data.insert("menu_item", "tasks_manual_generation");
            req.state().templates.render("page", &data).unwrap()
        };
        // We return the response
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(html)
    }

    /// Handles GET "/tasks"
    pub fn tasks(req: HttpRequest<ApplicationState>) -> HttpResponse {
        // We render the template
        let html = {
            let mut data = HashMap::new();
            data.insert("content", "tasks_overview_content");
            data.insert("menu_item", "tasks_overview");
            req.state().templates.render("page", &data).unwrap()
        };
        // We return the response
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(html)
    }

    /// Handles GET "/executions"
    pub fn executions_overview(req: HttpRequest<ApplicationState>) -> HttpResponse {
        // We render the template
        let html = {
            let mut data = HashMap::new();
            data.insert("content", "executions_overview_content");
            data.insert("menu_item", "reports_executions");
            req.state().templates.render("page", &data).unwrap()
        };
        // We return the response
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(html)
    }
}

// API
/// This modules contains handlers for api points.
mod api {

    use super::*;

    /// Handles GET "/api/get_executions_identifiers
    pub fn get_executions_identifiers(state: State<ApplicationState>) -> HttpResponse {
        // We retrieve the data
        let campaign = state.get_campaign();
        let executions_path = campaign.get_executions_path();
        let executions: Vec<String> = executions_path
            .read_dir()
            .unwrap()
            .map(|p| p.unwrap())
            .filter(|r| r.path().is_dir())
            .filter(|r| r.path().join(".excconf").exists())
            .map(|r| r.path().file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        // We return the response
        HttpResponse::Ok().json(executions)
    }

    /// Handles GET "/api/get_commits"
    pub fn get_commits(state: State<ApplicationState>) -> HttpResponse {
        // We retrieve the data
        let campaign = state.get_campaign();
        let commits = git::get_all_commits(&campaign.get_experiment_path()).unwrap();
        // We return the response
        HttpResponse::Ok().json(commits)
    }

    /// Handles GET "/api/get_profiles"
    pub fn get_profiles(_state: State<ApplicationState>) -> HttpResponse {
        // We retrieve the data
        let runaway_path = path::PathBuf::from(env!("HOME")).join(".runaway");
        let profiles: Vec<String> = fs::read_dir(runaway_path)
            .unwrap()
            .into_iter()
            .map(|d| d.unwrap())
            .filter(|d| d.path().extension() == Some(ffi::OsStr::new("yml")))
            .map(|d| d.path().file_stem().unwrap().to_str().unwrap().to_owned())
            .collect();
        // We return the response
        HttpResponse::Ok().json(profiles)
    }

   
    #[derive(Deserialize, Debug)]
    pub struct GenerateJobsRequest {
        commit_hash: String,
        execution_profile: String,
        repetitions: usize,
        parameters: String,
    }

    #[derive(Serialize)]
    pub struct PostResponse {
        success: bool,
        error: String,
    }

    /// Handles POST "/api/generate_jobs"
    pub fn generate_jobs((req, state): (Form<GenerateJobsRequest>, State<ApplicationState>)) -> HttpResponse {
        // We parse the parameters string
        let parameters: Vec<String> = misc::parse_parameters(req.parameters.as_str(), req.repetitions);
        // We add the tasks
        parameters
            .into_iter()
            .map(|s| {
                tasks::Task::new(
                    state.campaign.clone(),
                    s,
                    Some(req.commit_hash.clone()),
                    req.execution_profile.clone(),
                )
            }).for_each(|t| state.get_taskqueue().push(t));
        HttpResponse::build(http::StatusCode::OK).json(PostResponse {
            success: true,
            error: String::from(""),
        })
    }

    /// Handles GET "/api/get_workers"
    pub fn get_workers(state: State<ApplicationState>) -> HttpResponse {
        let workers = state.get_taskqueue().get_workers_string();
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(workers)
    }

    /// Handles GET "/api/get_tasks"
    pub fn get_tasks(state: State<ApplicationState>) -> HttpResponse {
        let tasks = state.get_taskqueue().get_tasks_string();
        HttpResponse::build(http::StatusCode::OK)
            .content_type("text/html")
            .body(tasks)
    }

    /// Handles GET "/api/get_executions"
    pub fn get_executions(state: State<ApplicationState>) -> HttpResponse {
        let executions = {
            let campaign = state.get_campaign();
            let executions = campaign.get_executions();
            executions
        };
        let executions: Vec<Vec<String>> = executions
            .iter()
            .map(|e| {
                let mut execution_data: Vec<String> = Vec::new();
                execution_data.push(String::from(e.get_identifier()));
                execution_data.push(String::from(e.get_commit()));
                execution_data.push(String::from(e.get_parameters()));
                execution_data.push(format!("{}", e.get_state()));
                execution_data.push(
                    e.get_execution_date()
                        .clone()
                        .unwrap_or(String::from("None")),
                );
                execution_data.push(e.get_execution_duration().map_or_else(
                    || String::from("None"),
                    |u| format!("{}:{}:{}", u / 3600, u % 3600 / 60, u % 3600 % 60),
                ));
                execution_data.push(e.get_executor().clone().unwrap_or(String::from("None")));
                execution_data.push(e.get_generator().to_owned());
                execution_data.push(
                    e.get_execution_exit_code()
                        .map_or_else(|| String::from("None"), |u| format!("{}", u)),
                );
                execution_data
            }).collect();
        let mut data = HashMap::new();
        data.insert("data", executions);
        return HttpResponse::build(http::StatusCode::OK).json(data);
    }

    #[derive(Deserialize)]
    pub struct DeleteExecutionsRequest{
        // The identifiers separated by a `¤` character.
        identifiers: String
    }

    /// Handles POST "/api/delete_executions"
    pub fn delete_execution((state, req): (State<ApplicationState>, Form<DeleteExecutionsRequest>)) -> HttpResponse {
        // We retrieve data
        let campaign = state.get_campaign();
        let execution_ids = req.identifiers.split("¤").collect::<Vec<_>>();
        // We retrieve executions results
        let executions = execution_ids
            .iter()
            .map(|id| repository::Execution::from_path(&campaign.get_executions_path().join(id)))
            .collect::<Vec<_>>();
        // If one execution is an error, we return a not found
        if executions.iter().any(|e| e.is_err()){
            return HttpResponse::build(http::StatusCode::NOT_FOUND).finish();
        }
        // We remove executions
        executions
            .into_iter()
            .map(|e| e.unwrap())
            .for_each(|e| campaign.remove_execution(e).unwrap());
        // We pull and push
        for _ in 0..5 {
            if campaign.pull().is_ok() && campaign.push().is_ok(){
                break
            }
                else{
                    warn!("Failed to push the executions removal. Retrying ...");
                }
        }
        // We return a OK
        return HttpResponse::build(http::StatusCode::OK).finish();
    }

    #[derive(Deserialize, Debug)]
    pub struct RescheduleExecutionsRequest{
        // The identifiers separated by a `¤` character.
        identifiers: String,
        // The profile to use.
        profile: String,
    }

    /// Handles POST "/api/reschedule_executions"
    pub fn reschedule_executions((state, req): (State<ApplicationState>, Form<RescheduleExecutionsRequest>)) -> HttpResponse {
        println!("{:?}", req);
        // We retrieve data
        let campaign = state.get_campaign();
        let execution_ids = req.identifiers.split("¤").collect::<Vec<_>>();
        // We retrieve executions results
        let executions = execution_ids
            .iter()
            .map(|id| repository::Execution::from_path(&campaign.get_executions_path().join(id)))
            .collect::<Vec<_>>();
        // If one execution is an error, we return a not found
        if executions.iter().any(|e| e.is_err()){
            return HttpResponse::build(http::StatusCode::NOT_FOUND).finish();
        }
        // We generate the tasks
        let rescheduled_tasks = executions
            .iter()
            .map(|e| e.as_ref().unwrap())
            .map(|e| tasks::Task::new(state.campaign.clone(),
                                      e.get_parameters().to_owned(),
                                      Some(e.get_commit().to_owned()),
                                      req.profile.clone()))
            .collect::<Vec<_>>();
        // We remove executions
        executions
            .into_iter()
            .map(|e| e.unwrap())
            .for_each(|e| campaign.remove_execution(e).unwrap());
        // We pull and push
        for _ in 0..5 {
            if campaign.pull().is_ok() && campaign.push().is_ok(){
                break
            }
                else{
                    warn!("Failed to push the executions removal. Retrying ...");
                }
        }
        // We push the tasks
        rescheduled_tasks
            .into_iter()
            .for_each(|t| state.get_taskqueue().push(t));
        // We return a OK
        return HttpResponse::build(http::StatusCode::OK).finish();
    }
}

/// Helper to load handlebars templates
pub fn load_handlebar_templates() -> handlebars::Handlebars{
    let mut hdbr = handlebars::Handlebars::new();
    hdbr.register_partial("sidebar", include_str!("../../../../assets/templates/sidebar.hbs")).unwrap();
    hdbr.register_partial("topbar", include_str!("../../../../assets/templates/topbar.hbs")).unwrap();
    hdbr.register_partial("home_content", include_str!("../../../../assets/templates/home_content.hbs"),).unwrap();
    hdbr.register_partial("start_content", include_str!("../../../../assets/templates/start_content.hbs")).unwrap();
    hdbr.register_partial("execution_content", include_str!("../../../../assets/templates/execution_content.hbs")).unwrap();
    hdbr.register_partial("tasks_overview_content", include_str!("../../../../assets/templates/tasks_overview_content.hbs")).unwrap();
    hdbr.register_partial("manual_generation_content",include_str!("../../../../assets/templates/manual_generation_content.hbs"),).unwrap();
    hdbr.register_partial("executions_overview_content", include_str!("../../../../assets/templates/executions_overview_content.hbs"),).unwrap();
    hdbr.register_template_string("page", include_str!("../../../../assets/templates/page.hbs")).unwrap();
    hdbr
}

/// Function that launche the orchestra application on a given campaign. The number of workers for
/// the executions can be parameterized through `n_workers`.
pub fn launch_orchestra(campaign:repository::Campaign, port: u32, n_workers: u32) {
    let campaign = Arc::new(Mutex::new(campaign));
    let taskqueue = Arc::new(Mutex::new(tasks::TaskQueue::new(n_workers)));
    server::new(move || {
        App::with_state(ApplicationState {campaign: campaign.clone(), 
                                          taskqueue: taskqueue.clone(), 
                                          templates: load_handlebar_templates()})
            .middleware(Logger::default())
            .resource("/home", |r| r.method(http::Method::GET).with(ui::home))
            .resource("/manual_generation", |r| {r.method(http::Method::GET).with(ui::manual_generation)})
            .resource("/executions", |r| {r.method(http::Method::GET).with(ui::executions_overview)})
            .resource("/executions/{execution_id}", |r| {r.method(http::Method::GET).with(ui::execution)})
            .resource("/tasks", |r| r.method(http::Method::GET).with(ui::tasks))
            .resource("/api/get_executions_identifiers", |r| { r.method(http::Method::GET).with(api::get_executions_identifiers)})
            .resource("/api/get_commits", |r| {r.method(http::Method::GET).with(api::get_commits)})
            .resource("/api/get_profiles", |r| {r.method(http::Method::GET).with(api::get_profiles) })
            .resource("/api/generate_jobs", |r| {r.method(http::Method::POST).with(api::generate_jobs)})
            .resource("/api/get_workers", |r| { r.method(http::Method::GET).with(api::get_workers) })
            .resource("/api/get_tasks", |r| { r.method(http::Method::GET).with(api::get_tasks) })
            .resource("/api/get_executions", |r| {r.method(http::Method::GET).with(api::get_executions)})
            .resource("/api/delete_executions", |r| {r.method(http::Method::POST).with(api::delete_execution)})
            .resource("/api/reschedule_executions", |r| {r.method(http::Method::POST).with(api::reschedule_executions)})
            .handler("/static", StaticFiles::new(campaign.lock().unwrap().get_executions_path()).unwrap())
    })
    .bind(format!("127.0.0.1:{}", port).as_str())
    .unwrap()
    .workers(10)
    .run();
}
