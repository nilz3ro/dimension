# Bridge-Agent Agreement Pattern


Bridge applications will be the first well-established way to work with this skeleteon version of dimension.

The pattern:

Bridges form a communication layer between a developer's workload and the dimension compute runner.

For example, you could build a telegram bot powered by an AI agent running in dimension by creating a telegram bridge. The bridge will expose two webhook endpoints.

1. The webhook that telegram calls whenever a message is sent to the telegram bot.

2. The webhook that the Dimension VM calls to update the bridge. This will be used to respond to users on telegram when things happen.

For this example we will create a telegram bridge that allows users to talk to the dimension-coder agent. When a message comes in to the bridge, it will run the coder-agent bundle in dimension and pass it a session ID and optional metadata about the telegram user chatting with the bot.

When the coding agent starts, a function should check to see if there's an object in MinIO named after the session ID, if there is, the function should fetch the session object (JSON) and populate the pi-agent-core agent's context object with the history. The history should include all tool calls and anything else that the agent will need to function properly on its next call, since it's stateless, nothing will be persisted inside of the micro VM, we will rely on minIO object storage for sessions.

pi-agent-core is an event-based agent framework. We can have a javascript function that posts to the telegram bridge's update webhook to notify the user. Check the pi-agent-core docs for an example.

When this milestone is complete, we will have a working end-to-end example:

1. edit and build the dimension-coder agent locally,
2. build the bundle locally with dimension build
3. push the bundle to workers with dimension push
4. send a message to the telegram bridge
5. receive a message in response from the agent through the bridge.

Note: we may need to consolidate and deduplicate build commands, I notice that there's
dimension build, dimension push, and dimension bundles rebuild and dimension bundles upload. Figure out which one of these are the latest and make sure it works.

Dimension should allow users to define their bundles with docker files, then build the docker image locally, then extract the rootfs from the docker image and add the relevant files like hyphae-init.


