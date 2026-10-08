"""Shared signal handling for gate processes that must reap their children."""

import contextlib
import signal


class Cancelled(BaseException):
    def __init__(self, signum):
        self.signum = signum
        super().__init__(signal.Signals(signum).name)


@contextlib.contextmanager
def cancellation_signals():
    def cancel(signum, _frame):
        # Ignore subsequent signals while children are being reaped.
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, signal.SIG_IGN)
        raise Cancelled(signum)

    previous = {sig: signal.signal(sig, cancel) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)
