// The C boundary owns no Rust memory. Configure/destroy outside the callback;
// processing and seek use upstream's already reserved storage.
#include "signalsmith-stretch.h"
#include <new>
#include <memory>
#include <vector>

using Stretch = signalsmith::stretch::SignalsmithStretch<float>;
extern "C" {
// Any channel count. Stereo is a record; eight is a separated record, one
// stereo pair per stem, stretched together so the stems stay phase-locked
// and their levels can be applied after the stretch rather than before it.
void *defalt_stretch_new_channels(unsigned rate, int channels) noexcept {
    try {
        auto s = std::unique_ptr<Stretch>(new Stretch(0));
        s->presetDefault(channels, float(rate), true);
        // One seek here, off the audio thread, so the pre-roll buffer seek
        // resizes is already the right size when the callback first seeks.
        int length = s->outputSeekLength(2.0f);
        std::vector<float> silence(size_t(length) + 1, 0.0f);
        std::vector<const float *> input(size_t(channels), silence.data());
        s->outputSeek(input.data(), length);
        s->reset();
        return s.release();
    } catch (...) { return nullptr; }
}
void *defalt_stretch_new(unsigned rate) noexcept { return defalt_stretch_new_channels(rate, 2); }
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
// The same two, with one pointer per channel for however many it was built with.
void defalt_stretch_seek_n(void *p, const float *const *inputs, int frames) noexcept {
    static_cast<Stretch *>(p)->outputSeek(inputs, frames);
}
void defalt_stretch_process_n(void *p, const float *const *inputs, int input_frames,
                              float *const *outputs, int output_frames) noexcept {
    static_cast<Stretch *>(p)->process(inputs, input_frames, outputs, output_frames);
}
}
