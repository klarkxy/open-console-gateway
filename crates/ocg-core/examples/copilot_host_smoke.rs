//! Manual local acceptance with a synthetic Key and explicit isolated directories.
use ocg_core::{
    copilot_application::*, crypto::StaticKeyCipher, db::Database,
    dsh_application::DshGatewaySecret, state::CoreStateInner,
};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let uninstall = args.first().is_some_and(|a| a == "uninstall");
    if uninstall {
        args.remove(0);
    }
    if args.first().is_some_and(|a| a == "package") {
        std::fs::write(&args[1], ocg_core::copilot_extension_package::vsix_bytes()?)?;
        return Ok(());
    }
    if args.len() != 4 {
        return Err(
            "Expected: <data-dir> <user-data-dir> <extensions-dir> <synthetic-model-url>".into(),
        );
    }
    let dir = PathBuf::from(&args[0]);
    std::fs::create_dir_all(&dir)?;
    let core = Arc::new(CoreStateInner::new(
        Database::open(dir.clone())?,
        dir,
        Arc::new(StaticKeyCipher::new("synthetic-smoke")),
    )?);
    ocg_core::copilot_application_host::register(&core);
    let host = core.copilot_application_host().ok_or("Host unavailable")?;
    let target = CopilotTarget {
        installation: Some("insiders".into()),
        profile: None,
        user_data_dir: Some(args[1].clone()),
        extensions_dir: Some(args[2].clone()),
    };
    let before = host(CopilotApplicationHostRequest::Inspect {
        target: target.clone(),
    })?;
    if uninstall {
        let result = host(CopilotApplicationHostRequest::Uninstall {
            target: target.clone(),
            expected_fingerprint: before.fingerprint.ok_or("Missing fingerprint")?,
        })?;
        assert!(
            !result.installed,
            "real CLI uninstall after SecretStorage deletion"
        );
        println!("{{\"nativeUninstall\":true}}");
        return Ok(());
    }
    let result = host(CopilotApplicationHostRequest::Install {
        target: target.clone(),
        expected_fingerprint: before.fingerprint.ok_or("Missing fingerprint")?,
        gateway_v1_url: args[3].clone(),
        secret: DshGatewaySecret::new("synthetic-host-key".into()),
    })?;
    assert!(result.installed, "real CLI package registration");
    let start = Instant::now();
    loop {
        let now = host(CopilotApplicationHostRequest::Inspect {
            target: target.clone(),
        })?;
        if now.status == CopilotStatus::Connected {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err("Installed but extension did not acknowledge activation".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("{{\"nativeInstaller\":true,\"activationAcknowledged\":true}}");
    Ok(())
}
