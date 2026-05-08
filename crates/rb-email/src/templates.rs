use std::collections::HashMap;

use minijinja::Environment;

use crate::{Email, EmailError};

const VERIFY_EMAIL_TXT: &str = include_str!("../templates/verify-email.txt");
const VERIFY_EMAIL_HTML: &str = include_str!("../templates/verify-email.html");
const RESET_PASSWORD_TXT: &str = include_str!("../templates/reset-password.txt");
const RESET_PASSWORD_HTML: &str = include_str!("../templates/reset-password.html");
const TENANT_INVITE_TXT: &str = include_str!("../templates/tenant-invite.txt");
const TENANT_INVITE_HTML: &str = include_str!("../templates/tenant-invite.html");

/// Pre-defined transactional email templates.
#[derive(Debug, Clone)]
pub enum EmailTemplate {
    VerifyEmail { link: String },
    ResetPassword { link: String },
    TenantInvite { link: String, tenant_name: String },
}

impl EmailTemplate {
    /// Subject line for this template variant.
    #[must_use]
    pub fn subject(&self) -> String {
        match self {
            Self::VerifyEmail { .. } => "Verify your email address".to_string(),
            Self::ResetPassword { .. } => "Reset your password".to_string(),
            Self::TenantInvite { tenant_name, .. } => {
                format!("You're invited to join {tenant_name}")
            }
        }
    }

    /// Render the template into `(plain_text, html)` bodies.
    ///
    /// # Errors
    ///
    /// Returns [`EmailError::Template`] if minijinja rendering fails.
    pub fn render(&self) -> Result<(String, String), EmailError> {
        match self {
            Self::VerifyEmail { link } => {
                let ctx: HashMap<&str, &str> = [("link", link.as_str())].into_iter().collect();
                render_pair(VERIFY_EMAIL_TXT, VERIFY_EMAIL_HTML, &ctx)
            }
            Self::ResetPassword { link } => {
                let ctx: HashMap<&str, &str> = [("link", link.as_str())].into_iter().collect();
                render_pair(RESET_PASSWORD_TXT, RESET_PASSWORD_HTML, &ctx)
            }
            Self::TenantInvite { link, tenant_name } => {
                let ctx: HashMap<&str, &str> = [
                    ("link", link.as_str()),
                    ("tenant_name", tenant_name.as_str()),
                ]
                .into_iter()
                .collect();
                render_pair(TENANT_INVITE_TXT, TENANT_INVITE_HTML, &ctx)
            }
        }
    }

    /// Render this template into a ready-to-send [`Email`].
    ///
    /// # Errors
    ///
    /// Returns [`EmailError::Template`] if rendering fails.
    pub fn to_email(&self, to: impl Into<String>) -> Result<Email, EmailError> {
        let subject = self.subject();
        let (text_body, html_body) = self.render()?;
        Ok(Email {
            to: to.into(),
            subject,
            text_body,
            html_body,
        })
    }
}

fn render_pair(
    txt_src: &str,
    html_src: &str,
    ctx: &HashMap<&str, &str>,
) -> Result<(String, String), EmailError> {
    let mut env = Environment::new();
    env.add_template("txt", txt_src)
        .map_err(|e| EmailError::Template(e.to_string()))?;
    env.add_template("html_body", html_src)
        .map_err(|e| EmailError::Template(e.to_string()))?;

    let txt = env
        .get_template("txt")
        .and_then(|t| t.render(ctx))
        .map_err(|e| EmailError::Template(e.to_string()))?;

    let html = env
        .get_template("html_body")
        .and_then(|t| t.render(ctx))
        .map_err(|e| EmailError::Template(e.to_string()))?;

    Ok((txt, html))
}
