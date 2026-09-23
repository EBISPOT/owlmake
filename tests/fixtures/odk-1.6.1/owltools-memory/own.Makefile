## Customize Makefile settings for heap

# A rule of the repository's own that runs owltools, as $(OWLTOOLS) spells it:
# with the configured Java heap size in front.
$(REPORTDIR)/heap-dv.txt: $(ONT).owl | $(REPORTDIR)
	$(OWLTOOLS) $< --silence-elk --run-reasoner -r elk -u > $@
