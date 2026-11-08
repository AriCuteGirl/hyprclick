# lagswitch: the root daemon and the CLI.
{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "lagswitch";
  version = (lib.importTOML ../Cargo.toml).package.version;
  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "Per-app network freezer and fake lag for Linux";
    mainProgram = "lagswitch";
    platforms = lib.platforms.linux;
  };
}
