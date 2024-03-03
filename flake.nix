{
  inputs.fenix = {
    inputs.nixpkgs.follows = "nixpkgs";
    url = github:nix-community/fenix;
  };
  inputs.rust-manifest = {
    flake = false;
    url = "https://static.rust-lang.org/dist/2024-02-29/channel-rust-nightly.toml";
  };
  inputs.flake-utils.url = github:numtide/flake-utils;
  description = "runa - wayland compositor toolkit";

  outputs = { self, nixpkgs, fenix, flake-utils, ... } @ inputs: flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs { inherit system; overlays = [ fenix.overlays.default ]; };
      rust-toolchain = (pkgs.fenix.fromManifestFile inputs.rust-manifest).withComponents [
        "rustfmt"
        "rust-src"
        "clippy"
        "rustc"
        "cargo"
      ];
      rustPlatform = pkgs.makeRustPlatform {
        cargo = rust-toolchain;
        rustc = rust-toolchain;
      };
      runtimeDependencies = (with pkgs; [ libGL vulkan-loader ]) ++
        (with pkgs.xorg; [ libX11 libXdmcp libXrender libXcursor libXau libxcb libXfixes libXrandr libXext libXi ]);
    in
    with pkgs; {
      packages = rec {
        crescent = rustPlatform.buildRustPackage rec {
          inherit runtimeDependencies;
          pname = "crescent";
          version = "0.1.0";
          src = ./.;
          buildInputs = [ libxkbcommon gccForLibs.lib ];
          nativeBuildInputs = [ autoPatchelfHook ];
          postUnpack = ''
            echo $(pwd)
            ls $sourceRoot/runa-wayland-protocols/spec
            cp -v ${cargoLock.lockFile} $sourceRoot/Cargo.lock
          '';
          cargoLock.lockFile = ./nix/Cargo.lock;
          LIBCLANG_PATH = lib.makeLibraryPath [ llvmPackages_17.libclang.lib ];
          BINDGEN_EXTRA_CLANG_ARGS =
            # Includes with normal include path
            (builtins.map (a: ''-I"${a}/include"'') [
              linuxHeaders
            ]) ++ [
              ''-isystem "${pkgs.llvmPackages_latest.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages_latest.libclang.version}/include"''
              ''-isystem "${glibc.dev}/include"''
            ];
        };
        default = crescent;
      };
      devShells.default = mkShell {
        nativeBuildInputs = [ pkg-config cmake rust-toolchain ];
        buildInputs = [ libxkbcommon ];
        shellHook = ''
          export LD_LIBRARY_PATH="${lib.makeLibraryPath (runtimeDependencies ++ [ wayland ])}:$LD_LIBRARY_PATH"
        '';
        LIBCLANG_PATH = lib.makeLibraryPath [ llvmPackages_17.libclang.lib ];

        BINDGEN_EXTRA_CLANG_ARGS =
          # Includes with normal include path
          (builtins.map (a: ''-I"${a}/include"'') [
            linuxHeaders
          ]) ++ [
            ''-isystem "${pkgs.llvmPackages_latest.libclang.lib}/lib/clang/${lib.versions.major pkgs.llvmPackages_latest.libclang.version}/include"''
            ''-isystem "${glibc.dev}/include"''
          ];
      };
    });
}
