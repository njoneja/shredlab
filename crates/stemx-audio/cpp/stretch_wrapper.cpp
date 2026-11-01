// Minimal C ABI around Signalsmith Stretch (MIT, vendored in third_party/).
// Planar float buffers: inputs[channel][frame].

#include "signalsmith-stretch/signalsmith-stretch.h"

#include <cstddef>

using Stretch = signalsmith::stretch::SignalsmithStretch<float>;

extern "C" {

void *stemx_stretch_new(int channels, float sample_rate) {
    auto *s = new Stretch(12345);  // fixed seed: deterministic renders
    s->presetDefault(channels, sample_rate);
    return s;
}

void stemx_stretch_free(void *s) { delete static_cast<Stretch *>(s); }

void stemx_stretch_reset(void *s) { static_cast<Stretch *>(s)->reset(); }

int stemx_stretch_input_latency(void *s) { return static_cast<Stretch *>(s)->inputLatency(); }

int stemx_stretch_output_latency(void *s) { return static_cast<Stretch *>(s)->outputLatency(); }

int stemx_stretch_block_samples(void *s) { return static_cast<Stretch *>(s)->blockSamples(); }

int stemx_stretch_interval_samples(void *s) { return static_cast<Stretch *>(s)->intervalSamples(); }

void stemx_stretch_set_transpose_semitones(void *s, float semitones) {
    static_cast<Stretch *>(s)->setTransposeSemitones(semitones);
}

void stemx_stretch_seek(void *s, const float *const *inputs, int input_samples, double rate) {
    static_cast<Stretch *>(s)->seek(inputs, input_samples, rate);
}

void stemx_stretch_process(void *s, const float *const *inputs, int input_samples, float *const *outputs,
                           int output_samples) {
    static_cast<Stretch *>(s)->process(inputs, input_samples, outputs, output_samples);
}

}  // extern "C"
