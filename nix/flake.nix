{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
  };
  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
  }:
    flake-utils.lib.eachDefaultSystem
    (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
        with pkgs; {
          devShells.default = mkShell {
            buildInputs = [rustToolchain cargo-flamegraph perf libclang cmake protobuf openssl zstd awscli2 kubectl];
            RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
            LIBCLANG_PATH = pkgs.lib.makeLibraryPath [pkgs.llvmPackages_latest.libclang.lib];
          };
        }
    );
}
# # Add glibc, clang, glib, and other headers to bindgen search path
# BINDGEN_EXTRA_CLANG_ARGS =
# # Includes normal include path
# (builtins.map (a: ''-I"${a}/include"'') [
#   # add dev libraries here (e.g. pkgs.libvmi.dev)
#   pkgs.glibc.dev
# ])
# # Includes with special directory paths
# ++ [
#   ''-I"${pkgs.llvmPackages_latest.libclang.lib}/lib/clang/${pkgs.llvmPackages_latest.libclang.version}/include"''
#   ''-I"${pkgs.glib.dev}/include/glib-2.0"''
#   ''-I${pkgs.glib.out}/lib/glib-2.0/include/''
# ];

