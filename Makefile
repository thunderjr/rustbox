.PHONY: setup run rootfs

setup:
	docker build -t rustbox-node24:latest -f images/node24/Dockerfile .
	docker build -t rustbox-node22:latest -f images/node22/Dockerfile .
	docker build -t rustbox-python313:latest -f images/python313/Dockerfile .

rootfs: setup
	./scripts/build-rootfs.sh rustbox-node24:latest images/node24.ext4 2048
	./scripts/build-rootfs.sh rustbox-node22:latest images/node22.ext4 2048
	./scripts/build-rootfs.sh rustbox-python313:latest images/python313.ext4 2048

run:
	RUST_LOG=debug cargo run --bin rustboxd
