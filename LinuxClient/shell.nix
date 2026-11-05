# Development shell for the Linux client on NixOS: `nix-shell` in this folder.
{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ cargo rustc gcc clippy pkg-config clang llvmPackages.libclang ];
  buildInputs = with pkgs; [
    openssl ffmpeg-full libva libva-utils intel-media-driver vulkan-loader vulkan-tools
    wayland libxkbcommon libx11 libxcursor libxi libxrandr
  ];
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.glibc.dev}/include -isystem ${pkgs.llvmPackages.libclang.lib}/lib/clang/${pkgs.lib.versions.major pkgs.llvmPackages.libclang.version}/include";
  shellHook = ''
    export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ pkgs.vulkan-loader pkgs.wayland pkgs.libxkbcommon pkgs.libx11 pkgs.libxcursor pkgs.libxi pkgs.libxrandr pkgs.libva ]}:/run/opengl-driver/lib:$LD_LIBRARY_PATH
    # VAAPI on Intel needs the iHD driver (not on the default path on NixOS).
    export LIBVA_DRIVERS_PATH=${pkgs.intel-media-driver}/lib/dri
    export LIBVA_DRIVER_NAME=''${LIBVA_DRIVER_NAME:-iHD}
  '';
}
