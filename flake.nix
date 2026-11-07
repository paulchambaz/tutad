{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = nixpkgs.legacyPackages.${system};

      buildPkgs = with pkgs; [
        pkg-config
        scdoc
      ];

      devPkgs = with pkgs; [
        cargo
        cargo-watch
        rustfmt
        clippy
        rustc
        just
        cargo-tarpaulin
      ];
    in {
      packages.default = pkgs.rustPlatform.buildRustPackage {
        pname = "tutad";
        version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
        src = ./.;
        cargoLock = {
          lockFile = ./Cargo.lock;
          outputHashes = pkgs.lib.genAttrs [
            "uniffi-0.27.1"
            "uniffi_bindgen-0.27.1"
            "uniffi_build-0.27.1"
            "uniffi_checksum_derive-0.27.1"
            "uniffi_core-0.27.1"
            "uniffi_macros-0.27.1"
            "uniffi_meta-0.27.1"
            "uniffi_testing-0.27.1"
            "uniffi_udl-0.27.1"
            "weedle2-5.0.0"
          ] (_: "sha256-3IH/e5DOPl2O3mJVKj9Ar60rRvSdDt4JUZ6SFEaiwcc=");
        };

        nativeBuildInputs = buildPkgs;

        postInstall = ''
          mkdir -p $out/share/man/man1
          SOURCE_DATE_EPOCH=${toString self.lastModified} scdoc < tutad.1.scd > $out/share/man/man1/tutad.1
        '';

        meta = {
          description = "Tuta to Maildir daemon";
          homepage = "https://github.com/paulchambaz/tutad";
          license = pkgs.lib.licenses.gpl3Only;
          mainProgram = "tutad";
        };
      };

      devShells.default = pkgs.mkShell {
        nativeBuildInputs = buildPkgs;
        buildInputs = devPkgs;
      };
    });
}
