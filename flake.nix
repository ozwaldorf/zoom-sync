{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
    in
    {

      packages.${system}.default = (
        pkgs.callPackage (
          { lib, rustPlatform }:
          rustPlatform.buildRustPackage {
            name = "zoom65-sync";
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
            };
            nativeBuildInputs = with pkgs; [ pkg-config ];
            buildInputs = with pkgs; [
              systemd # for libudev
              openssl
            ];

          }
        ) { }
      );
      devShells.${system}.default = pkgs.mkShell {
        packages = with pkgs; [
          rustup
          rust-analyzer
        ];
        inputsFrom = [ self.packages.${system}.default ];
      };
    };
}