//! runaway-cli/logger.rs
//! Author: Alexandre Péré
//!


//------------------------------------------------------------------------------------------- IMPORT


use tracing::{Subscriber, Level, Metadata, Id, Event, span, field};
use span::{Attributes, Record};
use field::{Visit, Field};


//--------------------------------------------------------------------------------------------- TYPE


pub struct EchoVisitor;

impl Visit for EchoVisitor{
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug){
        if field.name() ==  "message"{
            let message = format!("{:?}", value).replace("\n", "\n         ");
            eprintln!("runaway: {}", message)
        }
    }
}

// This logger only echoes messages to stderr, without any more formatting.
pub enum EchoSubscriber{
    Normal,
    Verbose,
    Ssh,
    Trace(String)
}

impl EchoSubscriber{
    pub fn setup_normal(){
        tracing::subscriber::set_global_default(EchoSubscriber::Normal)
            .expect("Setting logger failed.");
    }
    pub fn setup_verbose(){
        tracing::subscriber::set_global_default(EchoSubscriber::Verbose)
            .expect("Setting logger failed.");
    }
    pub fn setup_ssh(){
        tracing::subscriber::set_global_default(EchoSubscriber::Ssh)
            .expect("Setting logger failed.");
    }
    pub fn setup_trace(module: String){
        tracing::subscriber::set_global_default(EchoSubscriber::Trace(module))
            .expect("Setting logger failed.");

    }
}

impl Subscriber for EchoSubscriber{
    fn enabled(&self, metadata: &Metadata) -> bool { 
        match (self, metadata.level(), metadata.target()){
            (_, &Level::INFO, _) => true,
            (_, &Level::ERROR, _)  => true,
            (EchoSubscriber::Verbose, &Level::DEBUG, t) if t != "liborchestra::ssh" => true,
            (EchoSubscriber::Verbose, &Level::WARN, _) => true,
            (EchoSubscriber::Ssh, &Level::DEBUG, "liborchestra::ssh") => true,
            (EchoSubscriber::Ssh, &Level::WARN, "liborchestra::ssh") => true,
            (EchoSubscriber::Trace(m), &Level::TRACE, t) if t == format!("liborchestra::{}", m) => true,
            (EchoSubscriber::Trace(_), &Level::DEBUG, _) => true,
            _ => false
        }
    }

    fn new_span(&self, _span: &Attributes) -> Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event) {
        event.record(&mut EchoVisitor);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

