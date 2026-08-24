variable "enable_deployment" {
  description = "Hard opt-in. False creates a zero-resource plan."
  type        = bool
  default     = false
}

variable "deployment_acknowledgement" {
  description = "Must equal the documented acknowledgement before resources can be enabled."
  type        = string
  default     = ""
}

variable "project_id" {
  description = "Existing Google Cloud project ID."
  type        = string
  default     = "inkbridge-not-configured"
}

variable "project_number" {
  description = "Numeric project number, used for the Cloud Storage service agent IAM grant."
  type        = string
  default     = "000000000000"
}

variable "region" {
  description = "One region shared by the bucket, Eventarc trigger, Cloud Run, and Firestore."
  type        = string
  default     = "us-central1"
}

variable "bucket_name" {
  description = "Globally unique private Cloud Storage bucket name."
  type        = string
  default     = "inkbridge-not-configured"
}

variable "cloud_build_source_bucket_name" {
  description = "Globally unique private bucket used only for transient Cloud Build source archives."
  type        = string
  default     = "inkbridge-build-source-not-configured"
}

variable "cloud_run_image" {
  description = "Immutable linux/amd64 Artifact Registry digest. Empty performs the bootstrap stage without Cloud Run or Eventarc."
  type        = string
  default     = ""

  validation {
    condition = (
      var.cloud_run_image == "" ||
      can(regex("^[a-z0-9-]+-docker\\.pkg\\.dev/[^/]+/[^/]+/[^@]+@sha256:[0-9a-f]{64}$", var.cloud_run_image))
    )
    error_message = "cloud_run_image must be empty for bootstrap or an immutable Artifact Registry @sha256 digest."
  }
}

variable "artifact_repository" {
  description = "Regional Artifact Registry Docker repository used for InkBridge runtime images."
  type        = string
  default     = "inkbridge"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,62}$", var.artifact_repository))
    error_message = "artifact_repository must be a lowercase Artifact Registry repository ID."
  }
}

variable "firestore_database" {
  description = "Firestore Native database ID."
  type        = string
  default     = "(default)"
}

variable "folder_transport_operator" {
  description = "Operator IAM member (for example user:name@example.com) allowed to impersonate the folder transport and invoke the private conflict API; required for runtime deployment."
  type        = string
  default     = ""

  validation {
    condition = (
      var.folder_transport_operator == "" ||
      can(regex("^(user|group):[^[:space:]@]+@[^[:space:]@]+$", var.folder_transport_operator))
    )
    error_message = "folder_transport_operator must be empty or a user:/group: IAM member."
  }
}

variable "monthly_budget_usd" {
  description = "Optional billing budget. Zero omits it. Budgets alert but do not cap charges."
  type        = number
  default     = 0
}

variable "billing_account" {
  description = "Billing account ID required only when monthly_budget_usd is non-zero."
  type        = string
  default     = ""
  sensitive   = true
}
