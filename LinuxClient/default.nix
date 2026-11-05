# nix-build                      -> ./result/bin/sill
# pkgs.callPackage ./package.nix { }  -> from a NixOS config
{
  pkgs ? import <nixpkgs> { },
}:
pkgs.callPackage ./package.nix { }
