# The shared node of the VM checks, and the python their testScripts start
# with. The node enables the module with one release of the stub dsh (r1,
# the default) and has a lingering user, alice, and a user without linger,
# bob; two fixture user units that dshd must never touch, one of them
# holding a TCP port; sshd, so a check can start or attach from a session
# that then ends; and the ACP test client. Profiles and instances are made
# by the checks.
{ self, pkgs }:
let
  # Held by the bystander-web fixture: a web instance asking for it must
  # fail to start, and must not disturb the holder.
  heldPort = 41999;

  stub = version: import ./stub-dsh.nix { inherit pkgs version; };

  acpClient = pkgs.writers.writePython3Bin "acp-client" { } (builtins.readFile ./acp-client.py);

  fixture = command: {
    wantedBy = [ "default.target" ];
    serviceConfig.ExecStart = command;
  };
in
{
  inherit stub;

  prelude = "HELD_PORT = ${toString heldPort}\n" + builtins.readFile ./vm-prelude.py;

  node = {
    imports = [ self.nixosModules.default ];

    dshd = {
      enable = true;
      releases.r1 = {
        dsh = stub "1";
        unsetEnvironment = [ "SCRUB_PLANT" ];
      };
    };

    users.users.alice = {
      isNormalUser = true;
      linger = true;
    };
    users.users.bob.isNormalUser = true;

    services.openssh.enable = true;

    environment.systemPackages = [
      acpClient
      pkgs.jq
    ];

    systemd.user.services = {
      bystander-terminal = fixture "${pkgs.coreutils}/bin/sleep infinity";
      bystander-web = fixture "${pkgs.netcat}/bin/nc -lk 127.0.0.1 ${toString heldPort}";
    };

    virtualisation.memorySize = 1536;
  };
}
