terraform {
  required_version = ">= 1.8.0"

  # Real deployments configure this partial backend at init time with a
  # private, versioned bootstrap bucket. CI uses `terraform init -backend=false`.
  backend "gcs" {}

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = ">= 6.0, < 8.0"
    }
  }
}

provider "google" {
  project      = var.project_id
  region       = var.region
  access_token = local.enabled ? null : "deployment-disabled-no-api-call"
}
