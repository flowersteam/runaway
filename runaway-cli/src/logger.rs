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
    Verbose
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
}

impl Subscriber for EchoSubscriber{
    fn enabled(&self, metadata: &Metadata) -> bool { 
        match (self, metadata.level()){
            (_, &Level::INFO) | (_, &Level::ERROR)  => true,
            (EchoSubscriber::Verbose, &Level::DEBUG) | (EchoSubscriber::Verbose, &Level::WARN) => true,
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

