// Standalone audit executable: global new instrumentation is deliberately not
// linked into the application. Compile with the same optimized MSVC bridge.
// Covers both processors a deck owns: the stereo one for a record and the
// eight channel one for a separated record (one stereo pair per stem).
#include <atomic>
#include <cstdlib>
#include <new>
#include <cstdio>
#include <cmath>
#include <vector>
#include <chrono>
static bool audit = false;
static size_t allocations = 0;
void *operator new(size_t n) {
    if (audit) ++allocations;
    if (void *p = std::malloc(n)) return p;
    throw std::bad_alloc();
}
void *operator new[](size_t n) { return ::operator new(n); }
void operator delete(void *p) noexcept { std::free(p); }
void operator delete[](void *p) noexcept { std::free(p); }
void operator delete(void *p, size_t) noexcept { std::free(p); }
void operator delete[](void *p, size_t) noexcept { std::free(p); }
#include "../src/engine/stretch_bridge.cpp"

static double run(int channels) {
    void *stretch = defalt_stretch_new_channels(48000, channels);
    if (!stretch) std::exit(2);
    int count = defalt_stretch_seek_length(stretch, 2);
    std::vector<std::vector<float>> input(channels, std::vector<float>(count));
    std::vector<std::vector<float>> output(channels, std::vector<float>(256));
    std::vector<const float *> in(channels);
    std::vector<float *> out(channels);
    for (int c = 0; c < channels; ++c) {
        for (int i = 0; i < count; ++i) input[c][i] = 0.2f*std::sin(i*(0.06f + 0.01f*c));
        in[c] = input[c].data();
        out[c] = output[c].data();
    }
    audit = true;
    auto began = std::chrono::steady_clock::now();
    for (int repeat = 0; repeat < 4; ++repeat) {
        // A seek at a different rate every time, as a hot cue, a loop seam
        // under key lock and a roll release all do.
        defalt_stretch_seek_n(stretch, in.data(), defalt_stretch_seek_length(stretch, 0.8f + 0.3f*repeat));
        for (int i = 0; i < 1000; ++i) {
            int length = 236 + i%41; // changing tempo, through silence and signal
            if (i == 300) for (auto &channel : input) for (auto &v : channel) v = 0;
            if (i == 700) for (int c = 0; c < channels; ++c)
                for (int n = 0; n < count; ++n) input[c][n] = 0.2f*std::sin(n*0.05f);
            defalt_stretch_process_n(stretch, in.data(), length, out.data(), 256);
        }
    }
    auto elapsed = std::chrono::duration<double>(std::chrono::steady_clock::now() - began).count();
    audit = false;
    defalt_stretch_delete(stretch);
    return elapsed;
}

int main() {
    double stereo = run(2);
    size_t stereo_allocations = allocations;
    double stems = run(8);
    std::printf("C++ callback allocations: %zu stereo, %zu eight channel; processed 21.33 seconds "
                "in %.3f s (stereo) and %.3f s (stems)\n",
                stereo_allocations, allocations - stereo_allocations, stereo, stems);
    return allocations ? 1 : 0;
}
