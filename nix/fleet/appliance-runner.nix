{ pkgs, guest, ... }:
pkgs.writeShellApplication {
  name = "cas-fleet-appliance";
  runtimeInputs = [ pkgs.python3 ];
  text = ''export PYTHONDONTWRITEBYTECODE=1; exec python3 ${../../experiments/two-host}/appliance.py --vm ${guest.config.system.build.vm}/bin/run-cas-fleet-appliance-vm "$@"'';
}
