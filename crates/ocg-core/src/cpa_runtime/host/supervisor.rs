//! Private, transient Unix mode of the existing Host executable.
//!
//! The supervisor anchors the process group until its final group SIGKILL.
//! Its stdin is the lifetime pipe; CPA gets /dev/null instead, so it cannot
//! consume shutdown notifications. No group identifier crosses back to Host.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::signal::{SigAction, SigHandler, SigSet, Signal, killpg, sigaction};
use nix::unistd::{getpgrp, getpid};

const MARKER: &str = "--ocg-internal-cpa-supervisor";
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

pub(super) fn command(executable: &Path, config: &Path, device: bool) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    command.arg(MARKER).arg(executable).arg(config);
    #[cfg(not(test))]
    if device {
        command.arg("--codex-device-login");
    }
    // Rust's unit-test executable has its own main. Re-enter one exact test
    // instead; it calls the identical supervisor loop without an app runtime.
    #[cfg(test)]
    command
        .args([
            "--exact",
            "cpa_runtime::host::tests::unix_supervisor_entry",
            "--nocapture",
        ])
        .env("OCG_CPA_TEST_EXECUTABLE", executable)
        .env("OCG_CPA_TEST_CONFIG", config)
        .env("OCG_CPA_TEST_DEVICE", if device { "1" } else { "0" });
    Ok(command)
}

pub(super) fn run_if_requested() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(MARKER)) {
        return;
    }
    let (Some(executable), Some(config)) = (args.next(), args.next()) else {
        std::process::exit(2);
    };
    let device = match args.next() {
        None => false,
        Some(arg) if arg == "--codex-device-login" => true,
        _ => std::process::exit(2),
    };
    if args.next().is_some() {
        std::process::exit(2);
    }
    run(Path::new(&executable), Path::new(&config), device);
}

extern "C" fn request_stop(_: nix::libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
}

pub(super) fn run(executable: &Path, config: &Path, device: bool) -> ! {
    // Refuse a manual invocation in an existing shell/app group. From here to
    // SIGKILL we ourselves keep the group ID alive, even after reaping CPA.
    if getpid() != getpgrp() {
        std::process::exit(2);
    }
    let action = SigAction::new(
        SigHandler::Handler(request_stop),
        nix::sys::signal::SaFlags::empty(),
        SigSet::empty(),
    );
    for signal in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        // Only this private process changes signal dispositions. Caught
        // handlers reset to defaults when the CPA executable is exec'd.
        if unsafe { sigaction(signal, &action) }.is_err() {
            std::process::exit(2);
        }
    }

    let mut command = Command::new(executable);
    if device {
        command.args(["--codex-device-login", "--no-browser"]);
    }
    let mut child = match command
        .arg("--config")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            // Never include environment values or executable arguments.
            tracing::error!("failed to start owned CPA: {error}");
            std::process::exit(1);
        }
    };

    loop {
        if STOP_REQUESTED.load(Ordering::Relaxed) {
            break;
        }
        match child.try_wait() {
            Ok(None) => {}
            // A finished leader may still have descendants holding log pipes.
            Ok(Some(_)) | Err(_) => break,
        }
        let mut pipe = nix::libc::pollfd {
            fd: nix::libc::STDIN_FILENO,
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { nix::libc::poll(&mut pipe, 1, 20) };
        if result < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if result > 0 {
            if pipe.revents & (nix::libc::POLLHUP | nix::libc::POLLERR | nix::libc::POLLNVAL) != 0 {
                break;
            }
            let mut byte = 0u8;
            let read = unsafe {
                nix::libc::read(nix::libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1)
            };
            if read <= 0 {
                break;
            }
        }
    }

    // The supervisor catches its own TERM. CPA and descendants can terminate
    // gracefully, but neither leader exit nor retained stdout ends cleanup.
    let _ = killpg(getpgrp(), Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let _ = child.try_wait();
        std::thread::sleep(Duration::from_millis(20));
    }
    // Include ourselves in the final signal: the group ID is still reserved
    // for this exact live group at the syscall, so PID reuse cannot retarget it.
    let _ = killpg(getpgrp(), Signal::SIGKILL);
    std::process::exit(1);
}
