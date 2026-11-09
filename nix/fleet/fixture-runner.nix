{
  pkgs,
  lib,
  guest,
}:
pkgs.writeShellApplication {
  name = "cas-two-host-fixture";
  runtimeInputs = [ pkgs.python3 ];
  text = ''export PYTHONDONTWRITEBYTECODE=1; exec python3 ${../../experiments/two-host}/fixture.py --vm ${guest.config.system.build.vm}/bin/run-cas-two-host-fixture-vm "$@"'';
}
