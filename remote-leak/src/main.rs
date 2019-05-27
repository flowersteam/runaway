#![feature(await_macro, async_await, futures_api)]
extern crate liborchestra;
use liborchestra::ssh;

use std::alloc::System;

#[global_allocator]
static GLOBAL: System = System;



fn main(){
    use futures::executor::block_on;
    for i in 1..100{
        async fn connect_and_ls() {
            let profile = ssh::config::SshProfile{
                name: "test".to_owned(),
                hostname: Some("127.0.0.1".to_owned()),
                user: Some("apere".to_owned()),
                port: None,
                proxycommand: Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
            };
            let remote = ssh::RemoteHandle::spawn_resource(profile).unwrap();
            let output = await!(remote.async_exec("echo kikou")).unwrap();
            println!("Executed and resulted in {:?}", String::from_utf8(output.stdout).unwrap());
        }
        println!("{}", i);
        block_on(connect_and_ls());
 }
}

