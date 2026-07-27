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
        # apple-sdk_26, not the default (14.4): the ISO 21496-1 gain-map API
        # this project is built on — kCGImageAuxiliaryDataTypeISOGainMap,
        # kCGImageDestinationEncodeToISOGainmap, kCGImageDestinationEncodeRequest
        # — first ships in the macOS 15 SDK, and ImageIO.tbd in 14.4 does not
        # export it, so anything linking tohdr-apple dies with "Undefined
        # symbols for architecture arm64". Exporting SDKROOT is not a
        # workaround: the nix cc wrapper passes its own -isysroot and ignores it.
        #
        # darwinMinVersionHook raises the deployment target to match. At the
        # nixpkgs default of 14.0 the linker weak-links those macOS 15 statics
        # rather than erroring, and they read back as NULL at runtime — a
        # quieter failure than the link error, and a worse one.
        ++ lib.optionals stdenv.isDarwin [
          apple-sdk_26
          (darwinMinVersionHook "15.0")
        ]
        ;
        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        # mold does not seem to use pkg-config with openssl.dev
        # LD_LIBRARY_PATH = "${pkgs.openssl.out}/lib";  XXX: causes glibc version mismatch
        # RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
      };
    });
}
