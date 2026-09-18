// The C boundary owns no Rust memory. Configure/destroy outside the callback;
// processing and seek use upstream's already reserved storage.
#include "signalsmith-stretch.h"
#include <new>
#include <memory>

using Stretch = signalsmith::stretch::SignalsmithStretch<float>;
extern "C" {
void *defalt_stretch_new(unsigned rate) noexcept {
    try {
        auto s = std::unique_ptr<Stretch>(new Stretch(0));
        s->presetDefault(2, float(rate), true);
        return s.release();
    } catch (...) { return nullptr; }
}
void defalt_stretch_delete(void *p) noexcept { delete static_cast<Stretch *>(p); }
int defalt_stretch_seek_length(void *p, float rate) noexcept {
    return static_cast<Stretch *>(p)->outputSeekLength(rate);
}
void defalt_stretch_seek(void *p, const float *left, const float *right, int frames) noexcept {
    const float *input[2] = {left, right};
    static_cast<Stretch *>(p)->outputSeek(input, frames);
}
void defalt_stretch_process(void *p, const float *left, const float *right, int input_frames,
                            float *out_left, float *out_right, int output_frames) noexcept {
    const float *input[2] = {left, right};
    float *output[2] = {out_left, out_right};
    static_cast<Stretch *>(p)->process(input, input_frames, output, output_frames);
}
}
