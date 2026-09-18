// Standalone audit executable: global new instrumentation is deliberately not
// linked into the application. Compile with the same optimized MSVC bridge.
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

int main() {
    void *stretch = defalt_stretch_new(48000);
    if (!stretch) return 2;
    int count = defalt_stretch_seek_length(stretch, 2);
    std::vector<float> left(count), right(count), out_l(256), out_r(256);
    for (int i = 0; i < count; ++i) left[i] = right[i] = 0.2f*std::sin(i*0.06f);
    audit = true;
    auto began = std::chrono::steady_clock::now();
    for (int repeat = 0; repeat < 4; ++repeat) {
        defalt_stretch_seek(stretch, left.data(), right.data(), defalt_stretch_seek_length(stretch, 1));
        for (int i = 0; i < 1000; ++i) {
            int length = 236 + i%41; // changing tempo, through silence and signal
            if (i == 300) { for (auto &v : left) v = 0; for (auto &v : right) v = 0; }
            if (i == 700) for (int n = 0; n < count; ++n) left[n] = 0.2f*std::sin(n*0.05f);
            defalt_stretch_process(stretch, left.data(), right.data(), length,
                                   out_l.data(), out_r.data(), 256);
        }
    }
    auto elapsed = std::chrono::duration<double>(std::chrono::steady_clock::now() - began).count();
    audit = false;
    std::printf("C++ callback allocations: %zu; processed 21.33 seconds in %.3f seconds\n", allocations, elapsed);
    defalt_stretch_delete(stretch);
    return allocations ? 1 : 0;
}
