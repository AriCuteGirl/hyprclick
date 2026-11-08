# hyprclick. egui opens the Wayland and GL libraries at runtime, so they go
# on the binary's RUNPATH.
{ lib, rustPlatform, libxkbcommon, libGL, wayland, vulkan-loader }:

let
  runtimeLibs = [ libxkbcommon libGL wayland vulkan-loader ];
in
rustPlatform.buildRustPackage {
  pname = "hyprclick";
  version = (lib.importTOML ../Cargo.toml).package.version;
  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../plugin/hyprclick.cpp ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/hyprclick
  '';

  meta = {
    description = "Background autoclicker with anti-AFK and lag tools for Hyprland, Sway and KDE Plasma";
    mainProgram = "hyprclick";
    platforms = lib.platforms.linux;
  };
}
