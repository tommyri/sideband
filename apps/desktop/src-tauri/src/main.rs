// Release builds on Windows are GUI programs, so they open no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    socialspeak_desktop_lib::run();
}
