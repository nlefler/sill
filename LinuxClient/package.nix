# Nix package for the Sill client for Linux (binary: `sill`).
#
# From a NixOS config:
#   environment.systemPackages = [ (pkgs.callPackage /path/to/sill/LinuxClient/package.nix { }) ];
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  ffmpeg-full,
  libva,
  intel-media-driver,
  vulkan-loader,
  libxkbcommon,
  wayland,
  libx11,
  libxcursor,
  libxrandr,
  libxi,
}:
let
  ignored = [ "target" "result" ];
  src = lib.cleanSourceWith {
    src = lib.cleanSource ./.;
    filter = p: _type: !(builtins.elem (baseNameOf p) ignored);
  };

  # winit, wgpu and VAAPI dlopen these at runtime, so RPATH does not find them.
  runtimeLibs = [
    vulkan-loader
    libva
    wayland
    libxkbcommon
    libx11
    libxcursor
    libxrandr
    libxi
  ];
in
rustPlatform.buildRustPackage {
  pname = "sill";
  version = "0.1.0";

  inherit src;
  cargoLock.lockFile = ./Cargo.lock;

  # ffmpeg-sys-next runs bindgen (libclang) against FFmpeg's headers.
  nativeBuildInputs = [
    pkg-config
    makeWrapper
    rustPlatform.bindgenHook
  ];
  buildInputs = [ ffmpeg-full libva ] ++ runtimeLibs;

  # The GPU and VAAPI tests need a display and a device; run them in the dev shell.
  doCheck = false;
  cargoBuildFlags = [ "--package" "sill-app" ];

  postInstall = ''
    install -Dm644 packaging/sill.desktop $out/share/applications/sill.desktop
    install -Dm644 packaging/sill.svg $out/share/icons/hicolor/scalable/apps/sill.svg
  '';

  # /run/opengl-driver/lib holds the GPU's Vulkan driver on NixOS. iHD is the Intel VAAPI driver;
  # SILL_DECODER=sw, LIBVA_DRIVER_NAME and LIBVA_DRIVERS_PATH override these.
  postFixup = ''
    wrapProgram $out/bin/sill \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}:/run/opengl-driver/lib \
      --set-default LIBVA_DRIVERS_PATH ${intel-media-driver}/lib/dri \
      --set-default LIBVA_DRIVER_NAME iHD
  '';

  meta = {
    description = "Sill client for Linux: stream a Mac's desktop or windows";
    license = lib.licenses.asl20;
    mainProgram = "sill";
    platforms = lib.platforms.linux;
  };
}
