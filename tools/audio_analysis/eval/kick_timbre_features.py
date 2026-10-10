"""Causal fixed-size timbre fingerprints for kick template experiments.

The feature is deliberately fixed and causal.  Each completed sample hop is
represented by a trailing Hann-windowed FFT, reduced to twenty contiguous
geometric frequency bands.  The current spectrum and its three preceding
spectra form one eighty-element fingerprint.
"""

from __future__ import annotations

from collections import deque
import numbers

import numpy as np

from .kick_tonal_experiment import _fft_size, _trailing_frames


_BAND_COUNT = 20
_HISTORY_HOPS = 4
_LOW_FREQUENCY_HZ = 45.0
_HIGH_FREQUENCY_HZ = 8000.0
_SILENCE_NORM = 1e-12


def _samples_array(samples: np.ndarray) -> np.ndarray:
    """Validate and return finite real mono samples as float64."""
    values = np.asarray(samples)
    if values.ndim != 1:
        raise ValueError("samples must be a one-dimensional mono array")
    if not np.issubdtype(values.dtype, np.number) or np.iscomplexobj(values):
        raise ValueError("samples must contain real numeric values")
    try:
        values = values.astype(np.float64, copy=False)
    except (TypeError, ValueError, OverflowError) as exc:
        raise ValueError("samples must contain real numeric values") from exc
    if not np.all(np.isfinite(values)):
        raise ValueError("samples must contain only finite values")
    return values


def _sample_rate(value: int) -> int:
    if isinstance(value, (bool, np.bool_)) or not isinstance(value, numbers.Real):
        raise ValueError("sample_rate must be a finite integer")
    if not np.isfinite(value) or int(value) != value:
        raise ValueError("sample_rate must be a finite integer")
    rate = int(value)
    if rate <= 16_000:
        raise ValueError("sample_rate must be greater than 16000 Hz")
    return rate


def _band_masks(sample_rate: int, fft_size: int) -> tuple[np.ndarray, ...]:
    edges = np.geomspace(
        _LOW_FREQUENCY_HZ, _HIGH_FREQUENCY_HZ, _BAND_COUNT + 1
    )
    frequencies = np.fft.rfftfreq(fft_size, d=1.0 / sample_rate)
    return tuple(
        (frequencies >= left)
        & (frequencies < right if index + 1 < len(edges) - 1 else frequencies <= right)
        for index, (left, right) in enumerate(zip(edges[:-1], edges[1:]))
    )


def _band_spectra(
    values: np.ndarray,
    sample_rate: int,
    hop: int,
    fft_size: int,
    start_frame: int,
    frame_count: int,
    window: np.ndarray,
    masks: tuple[np.ndarray, ...],
) -> np.ndarray:
    frames = _trailing_frames(values, start_frame, frame_count, hop, fft_size)
    spectrum = np.fft.rfft(frames * window[None, :], axis=1)
    power = np.abs(spectrum) ** 2
    bands = np.empty((frame_count, _BAND_COUNT), dtype=np.float64)
    for band, mask in enumerate(masks):
        bands[:, band] = power[:, mask].sum(axis=1)
    return np.power(bands, 0.25)


def timbre_features(
    samples: np.ndarray,
    sample_rate: int,
    hop: int,
    batch_hops: int = 256,
) -> np.ndarray:
    """Return one causal eighty-element fingerprint for every complete hop.

    The four twenty-band spectra are flattened oldest to newest.  The first
    three fingerprints use zero vectors for history before the file starts.
    Every complete fingerprint is normalized once over all eighty elements;
    numerical silence remains an all-zero fingerprint.
    """
    values = _samples_array(samples)
    rate = _sample_rate(sample_rate)
    if isinstance(hop, (bool, np.bool_)) or not isinstance(hop, numbers.Integral):
        raise ValueError("hop must be a positive integer")
    hop = int(hop)
    if hop <= 0:
        raise ValueError("hop must be a positive integer")
    if isinstance(batch_hops, (bool, np.bool_)) or not isinstance(
        batch_hops, numbers.Integral
    ):
        raise ValueError("batch_hops must be a positive integer")
    batch_hops = int(batch_hops)
    if batch_hops <= 0:
        raise ValueError("batch_hops must be a positive integer")

    count = len(values) // hop
    features = np.zeros((count, _BAND_COUNT * _HISTORY_HOPS), dtype=np.float64)
    if count == 0:
        return features

    fft_size = _fft_size(rate)
    window = np.hanning(fft_size)
    masks = _band_masks(rate, fft_size)
    history: deque[np.ndarray] = deque(maxlen=_HISTORY_HOPS - 1)

    for batch_start in range(0, count, batch_hops):
        frame_count = min(batch_hops, count - batch_start)
        bands = _band_spectra(
            values,
            rate,
            hop,
            fft_size,
            batch_start,
            frame_count,
            window,
            masks,
        )
        for offset, current in enumerate(bands):
            row = features[batch_start + offset]
            history_count = len(history)
            if history_count:
                history_start = (_HISTORY_HOPS - 1 - history_count) * _BAND_COUNT
                row[history_start:-_BAND_COUNT] = np.concatenate(
                    tuple(history)
                )
            row[-_BAND_COUNT:] = current
            norm = float(np.linalg.norm(row))
            if norm <= _SILENCE_NORM:
                row.fill(0.0)
            elif not np.isfinite(norm):
                raise ValueError("timbre feature contains non-finite values")
            else:
                row /= norm
            history.append(current.copy())
    return features


def _validated_match_arrays(
    query: np.ndarray,
    templates: np.ndarray,
    labels: np.ndarray,
    groups: np.ndarray,
) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    query_values = np.asarray(query)
    template_values = np.asarray(templates)
    if np.iscomplexobj(query_values) or np.iscomplexobj(template_values):
        raise ValueError("query and templates must contain real values")
    if not np.issubdtype(query_values.dtype, np.number) or not np.issubdtype(
        template_values.dtype, np.number
    ):
        raise ValueError("query and templates must contain numeric values")
    try:
        query_array = query_values.astype(np.float64, copy=False)
        template_array = template_values.astype(np.float64, copy=False)
    except (TypeError, ValueError, OverflowError) as exc:
        raise ValueError("query and templates must contain numeric values") from exc
    label_array = np.asarray(labels)
    group_array = np.asarray(groups)
    if query_array.ndim != 1:
        raise ValueError("query must be a one-dimensional feature vector")
    if template_array.ndim != 2:
        raise ValueError("templates must be a two-dimensional array")
    if template_array.shape[1] != query_array.shape[0]:
        raise ValueError("query and templates must have the same feature width")
    if label_array.ndim != 1 or label_array.shape[0] != template_array.shape[0]:
        raise ValueError("labels must have one entry per template")
    if label_array.dtype != np.dtype(bool):
        raise ValueError("labels must be boolean")
    if group_array.ndim != 1 or group_array.shape[0] != template_array.shape[0]:
        raise ValueError("groups must have one entry per template")
    if not np.issubdtype(query_array.dtype, np.number) or not np.all(
        np.isfinite(query_array)
    ):
        raise ValueError("query must contain only finite numeric values")
    if not np.all(np.isfinite(template_array)):
        raise ValueError("templates must contain only finite numeric values")
    if any(not isinstance(group, str) for group in group_array.tolist()):
        raise ValueError("groups must contain string track ids")
    return query_array, template_array, label_array, group_array


def match_templates(
    query: np.ndarray,
    templates: np.ndarray,
    labels: np.ndarray,
    groups: np.ndarray,
    exclude_group: str,
) -> dict[str, float | int]:
    """Match a query against positive and negative templates by cosine.

    All templates from ``exclude_group`` are removed before selecting either
    class.  Returned indices always refer to the original template array.
    """
    if not isinstance(exclude_group, str):
        raise ValueError("exclude_group must be a string track id")
    query_array, template_array, label_array, group_array = _validated_match_arrays(
        query, templates, labels, groups
    )
    query_norm = float(np.linalg.norm(query_array))
    if not np.isfinite(query_norm) or query_norm <= 0.0:
        raise ValueError("query must be non-zero")

    candidate_indices = np.flatnonzero(group_array != exclude_group)
    if candidate_indices.size == 0:
        raise ValueError("track exclusion removed every template")
    candidate_templates = template_array[candidate_indices]
    candidate_norms = np.linalg.norm(candidate_templates, axis=1)
    if not np.all(np.isfinite(candidate_norms)) or np.any(candidate_norms <= 0.0):
        raise ValueError("candidate templates must be non-zero")
    if not np.isfinite(query_norm):
        raise ValueError("query must be finite")

    similarities = (candidate_templates / candidate_norms[:, None]) @ (
        query_array / query_norm
    )
    if not np.all(np.isfinite(similarities)):
        raise ValueError("cosine similarities must be finite")
    positive_candidates = candidate_indices[label_array[candidate_indices]]
    negative_candidates = candidate_indices[~label_array[candidate_indices]]
    if positive_candidates.size == 0 or negative_candidates.size == 0:
        raise ValueError("both positive and negative candidate classes are required")

    positive_positions = np.flatnonzero(label_array[candidate_indices])
    negative_positions = np.flatnonzero(~label_array[candidate_indices])
    positive_position = positive_positions[int(np.argmax(similarities[positive_positions]))]
    negative_position = negative_positions[int(np.argmax(similarities[negative_positions]))]
    positive_index = int(candidate_indices[positive_position])
    negative_index = int(candidate_indices[negative_position])
    positive_similarity = float(similarities[positive_position])
    negative_similarity = float(similarities[negative_position])
    return {
        "positive_similarity": positive_similarity,
        "negative_similarity": negative_similarity,
        "positive_index": positive_index,
        "negative_index": negative_index,
        "margin": positive_similarity - negative_similarity,
    }


__all__ = ["match_templates", "timbre_features"]
