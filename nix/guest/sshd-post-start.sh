# Publish the guest host key so the harness can pin it for SSH.

cp /etc/ssh/ssh_host_ed25519_key.pub /results/host-key.tmp
mv /results/host-key.tmp /results/host-key.pub
