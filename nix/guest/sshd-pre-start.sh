# Copy the host-provided SSH key into place before sshd accepts connections.
# The key file lives on the 9p results share.

install -Dm600 /results/authorized_keys /etc/ssh/authorized_keys.d/root
