{
  description = "Deepdelve - an underground voxel world tech demo (Rust + wgpu)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/fa1373130d5a21c040a47c5dbe1913d59ed91808";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;
      runtimeLibs = with pkgs; [
        vulkan-loader
        wayland
        libxkbcommon
        libx11
        libxcursor
        libxi
        libxrandr
      ];

      # The Vulkan loader dlopens the GPU driver, and an old loader can't load a driver
      # built against a newer glibc. So everything here uses the Mesa from its own nixpkgs
      # and skips the system's implicit layers, which keeps old releases running after a
      # NixOS upgrade. docs/RELEASES.md has the tradeoffs.
      vulkanEnv = {
        VK_DRIVER_FILES = "${pkgs.mesa}/share/vulkan/icd.d";
        VK_LOADER_LAYERS_DISABLE = "~implicit~";
      };

      deepdelve = pkgs.rustPlatform.buildRustPackage {
        pname = "deepdelve";
        version = (lib.importTOML ./Cargo.toml).package.version;
        # Only what the game is made from, so changes to docs or tools don't rebuild it.
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            ./shaders
            ./textures
          ];
        };
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "--bin" "deepdelve" ];
        # `play release` runs the tests with the GPU available.
        doCheck = false;
        env.DEEPDELVE_PACKAGED = "1";
        nativeBuildInputs = [ pkgs.makeWrapper ];
        # Set rather than prefixed, so a dev shell's libraries can't leak into a release.
        postFixup = ''
          wrapProgram $out/bin/deepdelve \
            --set LD_LIBRARY_PATH ${lib.makeLibraryPath runtimeLibs} \
            --set VK_DRIVER_FILES ${vulkanEnv.VK_DRIVER_FILES} \
            --set VK_LOADER_LAYERS_DISABLE ${lib.escapeShellArg vulkanEnv.VK_LOADER_LAYERS_DISABLE}
        '';
        meta.mainProgram = "deepdelve";
      };

      play = pkgs.buildGoModule {
        pname = "play";
        version = "0";
        src = ./tools/play;
        vendorHash = "sha256-ADFXmDNdBXj/NUKJOkoAPN8OTUuq9R9hIzqkgL2ubvo=";
        nativeBuildInputs = [ pkgs.makeWrapper ];
        # Nix itself comes from the system, so that it matches the system's daemon.
        postFixup = ''
          wrapProgram $out/bin/play --prefix PATH : ${lib.makeBinPath [ pkgs.git ]}
        '';
        meta.mainProgram = "play";
      };
    in
    {
      packages.${system} = {
        default = deepdelve;
        inherit deepdelve play;
      };

      devShells.${system}.default = pkgs.mkShell (vulkanEnv // {
        packages = with pkgs; [
          rustc
          cargo
          clippy
          rustfmt
          rust-analyzer
          pkg-config
          vulkan-tools
          vulkan-validation-layers
          go
          gopls
        ];
        buildInputs = runtimeLibs;
        # These libraries are loaded with dlopen at run time. Recording their paths in
        # the binary lets it start from anywhere, not only from inside this shell.
        RUSTFLAGS = "-C link-arg=-Wl,-rpath,${lib.makeLibraryPath runtimeLibs}";
        LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        VK_LAYER_PATH = "${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d";
      });
    };
}
