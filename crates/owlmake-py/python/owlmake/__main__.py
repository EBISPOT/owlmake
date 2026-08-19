"""``python -m owlmake ...`` — dispatch argv to owlmake in-process."""
from ._runtime import main

raise SystemExit(main())
