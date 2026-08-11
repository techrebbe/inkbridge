# InkBridge Google Cloud blueprint

This Terraform is deliberately inert by default. `enable_deployment = false`
produces a zero-resource configuration, and enabling it also requires the exact
acknowledgement string. CI runs only `fmt`, `init -backend=false`, and `validate`.
It never runs `plan` or `apply`.

The future reviewed deployment creates a private versioned bucket, Firestore
Native database, private Cloud Run service, Eventarc finalized-object trigger,
least-scope service accounts, and (when configured) a billing budget. A budget
alerts; it is not a hard spending cap.

Before any real deployment:

1. Review project, region, service accounts, retention, and expected traffic.
2. Build and push an immutable image digest from `Dockerfile.cloud-runtime`.
3. Choose a globally unique bucket and confirm the numeric project ID.
4. Set a monthly budget and billing account if budget alerts are desired.
5. Inspect a saved Terraform plan together.
6. Only then set both:

   ```hcl
   enable_deployment          = true
   deployment_acknowledgement = "I_UNDERSTAND_THIS_CREATES_BILLABLE_RESOURCES"
   ```

Do not commit `.tfvars`, state files, access tokens, or service-account keys.
