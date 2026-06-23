# Credentials are intentionally NOT declared as variables: the DNSimple provider
# reads them straight from the environment (DNSIMPLE_ACCOUNT, DNSIMPLE_TOKEN), so
# the account token never becomes a Terraform value that could land in state or
# a plan log. Set them in your shell before running:
#
#   export DNSIMPLE_ACCOUNT=<account-id>
#   export DNSIMPLE_TOKEN=<account-api-token>
#
provider "dnsimple" {
  # Flip to the DNSimple sandbox (sandbox.dnsimple.com) for dry runs against a
  # throwaway zone. Also honours the DNSIMPLE_SANDBOX env var.
  sandbox = var.dnsimple_sandbox
}
