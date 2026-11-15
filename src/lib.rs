//! Local gateway: domain/application depend on ports, concrete adapters are wired by bootstrap.
pub mod accounts;
pub mod adapters;
pub mod application;
pub mod authentication;
pub mod bootstrap;
pub mod config;
pub mod domain;
pub mod logging;
pub mod ports;
