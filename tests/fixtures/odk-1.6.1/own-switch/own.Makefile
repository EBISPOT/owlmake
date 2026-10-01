## Customize Makefile settings for sw

# A switch of the repository's own, on unless a run turns it off.
BRI = true

ifeq ($(BRI),true)
bridge/sw-bridge.owl: $(SRC)
	$(ROBOT) convert -i $< -o $@
endif

# A slice reached only through a pattern, and so an intermediate.
$(TMPDIR)/%-slice.owl: $(SRC)
	$(ROBOT) convert -i $< -o $@

sw-%-extra.owl: $(TMPDIR)/%-slice.owl
	$(ROBOT) convert -i $< -o $@

.PHONY: extras
extras: sw-a-extra.owl bridge/sw-bridge.owl
