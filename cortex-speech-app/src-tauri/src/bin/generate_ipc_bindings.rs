//! Deterministic checked-in TypeScript IPC contract generator.

fn main() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments.next().ok_or_else(|| "usage: generate_ipc_bindings <output.ts>".to_string())?;
    if arguments.next().is_some() {
        return Err("usage: generate_ipc_bindings <output.ts>".to_string());
    }
    cortex_speech_app_lib::ipc_contract::export_typescript_bindings(output)
        .map_err(|error| format!("failed to export IPC bindings: {error}"))
}
