.PHONY: setup run

setup:
	docker build -t rustbox-node24:latest -f images/node24/Dockerfile .
	docker build -t rustbox-node22:latest -f images/node22/Dockerfile .
	docker build -t rustbox-python313:latest -f images/python313/Dockerfile .

run:
	RUST_LOG=debug cargo run --bin rustboxd
