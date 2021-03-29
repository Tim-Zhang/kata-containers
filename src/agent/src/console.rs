// Copyright (c) 2019 Ant Financial
// Copyright (c) 2021 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0
//

use crate::util;
use anyhow::{anyhow, Result};
use lazy_static;
use nix::fcntl::{self, FcntlArg, FdFlag, OFlag};
use nix::libc::{STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::socket::{self, AddressFamily, SockAddr, SockFlag, SockType};
use nix::sys::stat::Mode;
use nix::sys::wait;
use nix::unistd::{self, close, dup2, fork, setsid, ForkResult, Pid};
use rustjail::pipestream::PipeStream;
use slog::Logger;
use std::ffi::{CStr, CString};
use std::os::unix::io::{FromRawFd, RawFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as SyncMutex;

use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::select;
use tokio::sync::watch::Receiver;

const CONSOLE_PATH: &str = "/dev/console";

lazy_static! {
    static ref SHELLS: Arc<SyncMutex<Vec<String>>> = {
        let mut v = Vec::new();

        if !cfg!(test) {
            v.push("/bin/bash".to_string());
            v.push("/bin/sh".to_string());
        }

        Arc::new(SyncMutex::new(v))
    };
}

pub fn initialize() {
    lazy_static::initialize(&SHELLS);
}

pub async fn debug_console_handler(
    logger: Logger,
    port: u32,
    mut shutdown: Receiver<bool>,
) -> Result<()> {
    let logger = logger.new(o!("subsystem" => "debug-console"));

    let shells = SHELLS.clone();
    let shells = shells.lock().unwrap().to_vec();

    let shell = shells
        .iter()
        .find(|sh| PathBuf::from(sh).exists())
        .ok_or_else(|| anyhow!("no shell found to launch debug console"))?;

    if port > 0 {
        let listenfd = socket::socket(
            AddressFamily::Vsock,
            SockType::Stream,
            SockFlag::SOCK_CLOEXEC,
            None,
        )?;
        let addr = SockAddr::new_vsock(libc::VMADDR_CID_ANY, port);
        socket::bind(listenfd, &addr)?;
        socket::listen(listenfd, 1)?;

        let mut incoming = util::get_vsock_incoming(listenfd)?;

        loop {
            select! {
                _ = shutdown.changed() => {
                    info!(logger, "debug console got shutdown request");
                    break;
                }

                conn = incoming.next() => {
                    if let Some(conn) = conn {
                        // Accept a new connection
                        match conn {
                            Ok(stream) => {
                                match run_debug_console_vsock(logger.clone(), shell, stream, shutdown.clone()).await {
                                    Ok(_) => {
                                        info!(logger, "run_debug_console_vsock session finished");
                                    }
                                    Err(err) => {
                                        error!(logger, "run_debug_console_vsock failed: {:?}", err);
                                    }
                                }
                            }
                            Err(e) => {
                                error!(logger, "{:?}", e);
                            }
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    } else {
        let mut flags = OFlag::empty();
        flags.insert(OFlag::O_RDWR);
        flags.insert(OFlag::O_CLOEXEC);

        loop {
            let fd = fcntl::open(CONSOLE_PATH, flags, Mode::empty())?;
            let shutdown_for_inner = shutdown.clone();
            select! {
                _ = shutdown.changed() => {
                    info!(logger, "debug console got shutdown request");
                    break;
                }

                result = run_debug_console_serial(logger.clone(), shell, fd, shutdown_for_inner) => {
                   match result {
                       Ok(_) => {
                           info!(logger, "run_debug_console_shell session finished");
                       }
                       Err(err) => {
                           error!(logger, "run_debug_console_shell failed: {:?}", err);
                       }
                   }
                }
            }
        }
    };

    Ok(())
}

fn run_in_child(slave_fd: libc::c_int, shell: &str) -> Result<()> {
    // create new session with child as session leader
    setsid()?;

    // dup stdin, stdout, stderr to let child act as a terminal
    dup2(slave_fd, STDIN_FILENO)?;
    dup2(slave_fd, STDOUT_FILENO)?;
    dup2(slave_fd, STDERR_FILENO)?;

    // set tty
    unsafe {
        libc::ioctl(0, libc::TIOCSCTTY);
    }

    let cmd = CString::new(shell).unwrap();
    let args: Vec<&CStr> = vec![];

    // run shell
    let _ = unistd::execvp(cmd.as_c_str(), args.as_slice()).map_err(|e| match e {
        nix::Error::Sys(errno) => {
            std::process::exit(errno as i32);
        }
        _ => std::process::exit(-2),
    });

    Ok(())
}

async fn run_in_parent<T: AsyncRead + AsyncWrite>(
    logger: Logger,
    mut shutdown: Receiver<bool>,
    stream: T,
    pseudo: OpenptyResult,
    child_pid: Pid,
) -> Result<()> {
    info!(logger, "get debug shell pid {:?}", child_pid);

    let master_fd = pseudo.master;
    let _ = close(pseudo.slave);

    let (mut socket_reader, mut socket_writer) = tokio::io::split(stream);
    let (mut master_reader, mut master_writer) = tokio::io::split(PipeStream::from_fd(master_fd));

    loop {
        select! {
            _ = shutdown.changed() => {
                info!(logger, "got shutdown request");
                break;
            },
            res = tokio::io::copy(&mut master_reader, &mut socket_writer) => {
                debug!(
                    logger,
                    "master closed: {:?}", res
                );

                break;
            },
            res = tokio::io::copy(&mut socket_reader, &mut master_writer) => {
                info!(
                    logger,
                    "socket closed: {:?}", res
                );

                break;
            }
        }
    }

    let wait_status = wait::waitpid(child_pid, None);
    info!(logger, "debug console process exit code: {:?}", wait_status);

    Ok(())
}

async fn run_debug_console_vsock<T: AsyncRead + AsyncWrite>(
    logger: Logger,
    shell: &str,
    stream: T,
    shutdown: Receiver<bool>,
) -> Result<()> {
    let logger = logger.new(o!("subsystem" => "debug-console-shell"));

    let pseudo = openpty(None, None)?;
    let _ = fcntl::fcntl(pseudo.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC));
    let _ = fcntl::fcntl(pseudo.slave, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC));

    let slave_fd = pseudo.slave;

    match fork() {
        Ok(ForkResult::Child) => run_in_child(slave_fd, shell),
        Ok(ForkResult::Parent { child: child_pid }) => {
            run_in_parent(logger.clone(), shutdown.clone(), stream, pseudo, child_pid).await
        }
        Err(err) => Err(anyhow!("fork error: {:?}", err)),
    }
}

async fn run_debug_console_serial(
    logger: Logger,
    shell: &str,
    fd: RawFd,
    mut shutdown: Receiver<bool>,
) -> Result<()> {
    let cmd = tokio::process::Command::new(shell)
        .arg("-i")
        .kill_on_drop(true)
        .stdin(unsafe { Stdio::from_raw_fd(fd) })
        .stdout(unsafe { Stdio::from_raw_fd(fd) })
        .stderr(unsafe { Stdio::from_raw_fd(fd) })
        .spawn();

    let mut cmd = match cmd {
        Ok(c) => c,
        Err(_) => return Err(anyhow!("failed to spawn shell")),
    };

    select! {
        _ = shutdown.changed() => {
            info!(logger, "got shutdown request");
        }
        _ = cmd.wait() => {}
    }

    Ok(())
}

// BUG: FIXME:
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_setup_debug_console_no_shells() {
        // Guarantee no shells have been added
        // (required to avoid racing with
        // test_setup_debug_console_invalid_shell()).
        let shells_ref = SHELLS.clone();
        let mut shells = shells_ref.lock().unwrap();
        shells.clear();
        let logger = slog_scope::logger();

        let result = setup_debug_console(&logger, shells.to_vec(), 0);

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "no shell found to launch debug console"
        );
    }

    #[test]
    fn test_setup_debug_console_invalid_shell() {
        let shells_ref = SHELLS.clone();
        let mut shells = shells_ref.lock().unwrap();

        let dir = tempdir().expect("failed to create tmpdir");

        // Add an invalid shell
        let shell = dir
            .path()
            .join("enoent")
            .to_str()
            .expect("failed to construct shell path")
            .to_string();

        shells.push(shell);
        let logger = slog_scope::logger();

        let result = setup_debug_console(&logger, shells.to_vec(), 0);

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "no shell found to launch debug console"
        );
    }
}
