Vendored native audio dependencies (unmodified upstream headers):

- Signalsmith Stretch 1.3.2, commit `57b93f4e9206a089a45387eaa39bdc9f310d3308`
  from https://github.com/Signalsmith-Audio/signalsmith-stretch
- Signalsmith Linear 0.3.1, commit `5668673560146a9cfe38c25315071e3fd68c8317`
  from https://github.com/Signalsmith-Audio/linear (the Stretch CMake dependency version).

Both are MIT licensed; original license and README files are retained alongside
their headers. `build.rs` compiles the small C ABI bridge with the existing MSVC
toolchain on Windows. No network access is required to build these libraries.
