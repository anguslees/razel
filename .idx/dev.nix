# To learn more about how to use Nix to configure your environment
# see: https://firebase.google.com/docs/studio/customize-workspace
{ pkgs, ... }: {
  # Which nixpkgs channel to use.
  channel = "stable-25.05"; # or "unstable"

  # Use https://search.nixos.org/packages to find packages
  packages = [
    pkgs.clang
    pkgs.bazelisk
    pkgs.cargo
    pkgs.rustc
    pkgs.rustfmt
    pkgs.clippy
    pkgs.stdenv.cc
  ];

  # Sets environment variables in the workspace
  env = {};
  services = {
    docker = {
      enable = true;
    };
  };

  idx = {
    # Search for the extensions you want on https://open-vsx.org/ and use "publisher.id"
    extensions = [
      "github.vscode-pull-request-github"
      "google.geminicodeassist"
      "rust-lang.rust-analyzer"
      "tamasfe.even-better-toml"
      "serayuzgur.crates"
      "vadimcn.vscode-lldb"
    ];

    # Enable previews
    previews = {
      enable = false;
      previews = {
        # web = {
        #   # Example: run "npm run dev" with PORT set to IDX's defined port for previews,
        #   # and show it in IDX's web preview panel
        #   command = ["npm" "run" "dev"];
        #   manager = "web";
        #   env = {
        #     # Environment variables to set for your server
        #     PORT = "$PORT";
        #   };
        # };
      };
    };

    # Workspace lifecycle hooks
    workspace = {
      # Runs when a workspace is first created
      onCreate = {
        default.openFiles = [ "src/main.rs" ];
        # Example: install JS dependencies from NPM
        # npm-install = "npm install";
        cargo-fetch = "cargo fetch";
      };
      # Runs when the workspace is (re)started
      onStart = {
        # Example: start a background task to watch and re-build backend code
        # watch-backend = "npm run watch-backend";
        start-qdrant = "docker run -p 6333:6333 -p 6334:6334 -v $HOME/.cache/qdrant_storage:/qdrant/storage qdrant/qdrant";
      };
    };
  };
}
