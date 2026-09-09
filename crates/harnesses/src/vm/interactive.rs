//! SSH access to the same disposable guest after its automatic IO checks.
use super::*;

fn public_key(text: &str) -> io::Result<String> {
    let mut fields = text.split_whitespace();
    let kind = fields.next().unwrap_or_default();
    let data = fields.next().unwrap_or_default();
    if text.trim().lines().count() != 1
        || !matches!(
            kind,
            "ssh-ed25519"
                | "ssh-rsa"
                | "ecdsa-sha2-nistp256"
                | "ecdsa-sha2-nistp384"
                | "ecdsa-sha2-nistp521"
                | "sk-ssh-ed25519@openssh.com"
                | "sk-ecdsa-sha2-nistp256@openssh.com"
        )
        || data.is_empty()
    {
        return Err(io::Error::other(
            "expected one OpenSSH public key without options",
        ));
    }
    // Drop the comment; ssh-keygen checks the encoded key before we boot.
    Ok(format!("{kind} {data}\n"))
}

pub(super) fn prepare(
    args: &Args,
    results: &Path,
    env: &mut BTreeMap<OsString, OsString>,
) -> io::Result<()> {
    let key = args
        .ssh_key
        .as_ref()
        .expect("validated interactive arguments");
    let text = fs::read_to_string(key)?;
    let authorized = results.join("authorized_keys");
    fs::write(&authorized, public_key(&text)?)?;
    let mut command = logged_command(Path::new("ssh-keygen"), &args.output, "ssh-key.log", env)?;
    command.arg("-l").arg("-f").arg(&authorized);
    if !ManagedChild::spawn(&mut command)?
        .wait(Duration::from_secs(5))?
        .success()
    {
        return Err(io::Error::other("invalid SSH public key; see ssh-key.log"));
    }
    env.insert("CAS_SSH_PORT".into(), args.ssh_port.to_string().into());
    Ok(())
}

pub(super) fn ready(
    args: &Args,
    results: &Path,
    guest: &mut ManagedChild,
    mut daemon: Option<&mut ManagedChild>,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    loop {
        process::check_interrupt()?;
        if guest.poll()?.is_some() {
            return Err(io::Error::other(
                "guest exited before SSH readiness; see console.log",
            ));
        }
        if let Some(child) = daemon.as_mut()
            && child.poll()?.is_some()
        {
            return Err(io::Error::other(
                "daemon exited before SSH readiness; see daemon.log",
            ));
        }
        if results.join("completion.json").try_exists()?
            && results.join("host-key.pub").try_exists()?
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "guest did not become ready for SSH",
            ));
        }
        thread::sleep(process::POLL);
    }
}

pub(super) fn announce(args: &Args, results: &Path) -> io::Result<()> {
    let key = public_key(&fs::read_to_string(results.join("host-key.pub"))?)?;
    let known_hosts = args.output.join("known_hosts");
    fs::write(&known_hosts, format!("[127.0.0.1]:{} {key}", args.ssh_port))?;
    let path = args.output.to_string_lossy().replace('\'', "'\\''");
    println!(
        "Guest IO checks passed. Connect from another terminal:\n\
         cd '{path}' && ssh -o StrictHostKeyChecking=yes -o UserKnownHostsFile=known_hosts -p {} root@127.0.0.1\n\
         Run cas-poweroff in the guest to flush and finish. Ctrl-C aborts the session.",
        args.ssh_port,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_keys_options_and_multiple_keys_before_copying() {
        for text in [
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsecret",
            "command=evil ssh-ed25519 AAAA",
            "ssh-ed25519 AAAA\nssh-ed25519 BBBB",
            "ssh-ed25519",
            "",
        ] {
            assert!(public_key(text).is_err());
        }
        assert_eq!(
            public_key("ssh-ed25519 AAAA a comment\n").unwrap(),
            "ssh-ed25519 AAAA\n"
        );
    }
}
