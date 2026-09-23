## Customize Makefile settings for force

# A mapping set fetched again on every build, and merged into the released one:
# its rule needs `.FORCE`, which names no file and is always out of date.
$(MAPPINGDIR)/upstream.sssom.tsv: .FORCE
	wget "http://example.org/upstream.sssom.tsv" -O $@
