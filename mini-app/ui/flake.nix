{
  description = "LDEX mini-app UI (QML UI App for Logos Basecamp)";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    # Input name MUST match the dependency name in metadata.json so the
    # builder auto-resolves it (configuration.md: "keys matching dependency
    # names are passed as moduleDeps").
    #
    # Relative path. This requires the surrounding directory to be a git
    # repo (otherwise Nix in pure-eval mode resolves relative paths
    # against the store copy, which doesn't contain ../core). The repo
    # is a git repo on every public clone, so this is the portable form.
    # For non-git working copies, override at the call site:
    #   nix run --override-input ldex_core path:/abs/path/to/mini-app/core
    ldex_core.url = "path:../core";
  };

  outputs = inputs@{ logos-module-builder, ... }:
    logos-module-builder.lib.mkLogosQmlModule {
      src = ./.;
      configFile = ./metadata.json;
      flakeInputs = inputs;
    };
}
