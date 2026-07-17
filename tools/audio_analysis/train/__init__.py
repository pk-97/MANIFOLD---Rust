"""train -- dev-only Python trainer for the Audio Event Classifier
(docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md D7): trains, never ships. Lives
alongside `eval/` and `manifold_audio/` in tools/audio_analysis, runs under
the same bundled runtime, imports `eval` and `manifold_audio` one-way
(train -> eval/manifold_audio, never the reverse).
"""
