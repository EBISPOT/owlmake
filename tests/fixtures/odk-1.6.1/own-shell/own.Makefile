## Customize Makefile settings for shell

# The pieces a rule merges, listed by a substitution that runs owlmake's own
# `grep`.
PIECES = $(shell ls pieces/*.owl | grep -v skip.owl)

$(TMPDIR)/pieces.owl: $(PIECES)
	$(ROBOT) merge $(foreach p, $(PIECES), -i $(p)) -o $@

.PHONY: pieces
pieces: $(TMPDIR)/pieces.owl
