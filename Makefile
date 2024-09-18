.PHONY: metadata

metadata:
	subxt metadata --url wss://rpc.polkadot.io:443 -f bytes > metadata/polkadot.scale
	subxt metadata --url wss://polkadot-collectives-rpc.polkadot.io:443 -f bytes > metadata/collectives-polkadot.scale
	subxt metadata --url wss://polkadot-people-rpc.polkadot.io:443 -f bytes > metadata/people-polkadot.scale
	wget https://raw.githubusercontent.com/paritytech/polkadot-sdk/master/polkadot/node/service/chain-specs/polkadot.json -O metadata/polkadot.json
	wget https://github.com/paritytech/polkadot-sdk/raw/master/cumulus/parachains/chain-specs/collectives-polkadot.json -O metadata/collectives-polkadot.json
	wget https://github.com/paritytech/polkadot-sdk/raw/master/cumulus/parachains/chain-specs/people-polkadot.json -O metadata/people-polkadot.json
