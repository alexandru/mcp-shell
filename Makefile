.PHONY: tests build docker-build-local update-skills

tests:
	cd backend && cargo test --workspace --all-features --locked

build:
	cd backend && cargo build --locked --release --package backend --bin mcp-shell

docker-build-local:
	docker build -t backend ./backend

update-skills:
	npx --yes skills update rust-skills --project --yes
