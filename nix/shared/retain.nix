{ pkgs }:
pkgs.writeShellApplication {
  name = "cas-retain-shared-results";
  runtimeInputs = [ pkgs.coreutils ];
  text = builtins.readFile ./retain.sh;
}
