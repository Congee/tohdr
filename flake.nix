{
  description = "tohdr — HDR gain-map HEIC writer (ISO 21496-1)";

  inputs.nixpkgs.url      = "github:nixos/nixpkgs/nixos-unstable";
  inputs.flake-utils.url  = "github:numtide/flake-utils";
  inputs.rust-overlay.url = "github:oxalica/rust-overlay";

  outputs = { self, nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
    let
      overlays = [ (import rust-overlay) ];
      pkgs = import nixpkgs { inherit system overlays; };

      # One toolchain, so cargo's sysroot matches the rustc on PATH.
      # clippy is an extension, not a package -- absent unless named.
      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        extensions = [ "rust-src" "clippy" "rust-analyzer" ];
      };
    in {
      devShells.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [ rustToolchain pkg-config ];
        buildInputs = with pkgs; [
          openssl.dev
        ]
        # SDK 26, not the default 14.4, which lacks the ISO gain-map symbols.
        # SDKROOT will not do it -- the cc wrapper forces its own -isysroot.
        # Min version 15.0 too, or those symbols weak-link and read NULL.
        ++ lib.optionals stdenv.isDarwin [
          apple-sdk_26
          (darwinMinVersionHook "15.0")
        ]
        ;
        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
      };
    });
}
