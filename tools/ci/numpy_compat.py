"""Compatibility helpers needed by soundcard without SciPy or SoundFile."""

import numpy as np

_NUMPY_FROMSTRING = np.fromstring


def binary_fromstring(string, dtype=float, count=-1, *, sep=""):
    """Provide the binary ``numpy.fromstring`` mode removed in NumPy 2."""
    if sep:
        return _NUMPY_FROMSTRING(string, dtype=dtype, count=count, sep=sep)
    return np.frombuffer(string, dtype=dtype, count=count).copy()
