{
  description = "Deepdelve - an underground voxel world tech demo (Rust + wgpu)";

  # Pinned to the running NixOS revision on purpose: the Vulkan loader from this
  # shell dlopens the system's Mesa driver, so both must share the same glibc.
  # If the driver fails to load after a system upgrade, re-pin with:
  #   nix flake lock --override-input nixpkgs github:NixOS/nixpkgs/$(nixos-version --revision)
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/fa1373130d5a21c040a47c5dbe1913d59ed91808";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      runtimeLibs = with pkgs; [
        vulkan-loader
        wayland
        libxkbcommon
        libx11
        libxcursor
        libxi
        libxrandr
      ];
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          rustc
          cargo
          clippy
          rustfmt
          rust-analyzer
          pkg-config
          vulkan-tools
          vulkan-validation-layers
        ];
        buildInputs = runtimeLibs;
        # These libraries are loaded with dlopen at run time. Recording their paths in
        # the binary lets it start from anywhere, not only from inside this shell.
        RUSTFLAGS = "-C link-arg=-Wl,-rpath,${pkgs.lib.makeLibraryPath runtimeLibs}";
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        VK_LAYER_PATH = "${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d";
      };
    };
}
