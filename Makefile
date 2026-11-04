.PHONY: depoly deploy

# Keep the requested spelling; deploy is also available as an alias.
depoly:
	bash deploy/native/deploy.sh

deploy: depoly
