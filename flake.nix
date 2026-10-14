{
  description = "OpenShores Launcher";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      ...
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      packageJson = builtins.fromJSON (builtins.readFile ./package.json);
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };

      launcher = rustPlatform.buildRustPackage {
        pname = "openshores-launcher";
        inherit (packageJson) version;

        src = ./.;
        cargoRoot = "src-tauri";
        buildAndTestSubdir = "src-tauri";

        cargoLock.lockFile = ./src-tauri/Cargo.lock;

        # Embed frontendDist in the release binary. Without Tauri's
        # custom-protocol feature, WebviewUrl::App resolves to devUrl and the
        # packaged launcher tries to connect to Vite on 127.0.0.1:1420.
        cargoBuildFlags = [
          "--features"
          "tauri/custom-protocol"
        ];

        nativeBuildInputs = with pkgs; [
          pkg-config
          wrapGAppsHook3
        ];

        buildInputs = with pkgs; [
          gtk3
          openssl
          webkitgtk_4_1
        ];

        postInstall = ''
          wrapProgram "$out/bin/openshores-launcher" \
            --prefix PATH : ${pkgs.lib.makeBinPath [
              pkgs.procps
              pkgs.wineWow64Packages.stable
              pkgs.xdelta
            ]}
        '';

        meta = {
          description = packageJson.description;
          homepage = "https://github.com/Norway174/OpenShores-Launcher";
          license = pkgs.lib.licenses.mit;
          mainProgram = "openshores-launcher";
          platforms = [ system ];
        };
      };
    in
    {
      packages.${system} = {
        default = launcher;
        openshores-launcher = launcher;
      };

      apps.${system} = {
        default = {
          type = "app";
          program = "${launcher}/bin/openshores-launcher";
        };
        openshores-launcher = {
          type = "app";
          program = "${launcher}/bin/openshores-launcher";
        };
      };
    };
}
