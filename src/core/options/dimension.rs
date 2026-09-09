//! 向量维度类型与构造期校验。

use crate::core::error::{MnemeError, Result};

/// 向量维度。
///
/// 取值闭区间 `[1, 65536]`;构造时校验,内部不再使用裸整数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimension(u32);

impl Dimension {
    /// 允许的最小维度。
    pub const MIN: u32 = 1;
    /// 允许的最大维度。
    pub const MAX: u32 = 65_536;

    /// 校验并构造一个维度。
    ///
    /// # Arguments
    ///
    /// * `value` - 维度,必须落在 `[1, 65536]`。
    ///
    /// # Returns
    ///
    /// 合法时返回 `Ok(Dimension)`;越界时返回 `Err`,绝不静默截断。
    ///
    /// # Errors
    ///
    /// 越界时返回 [`MnemeError::Invalid`]。
    ///
    /// # Examples
    ///
    /// ```
    /// use mneme::Dimension;
    ///
    /// assert_eq!(Dimension::new(1536).unwrap().get(), 1536);
    /// assert!(Dimension::new(0).is_err());
    /// assert!(Dimension::new(65_537).is_err());
    /// ```
    pub fn new(value: u32) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(MnemeError::Invalid("维度必须在 1..=65536 之间"))
        }
    }

    /// 返回内部维度值。
    ///
    /// # Returns
    ///
    /// 构造时校验通过的维度值。
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_accepts_bounds_and_rejects_outside() {
        assert_eq!(Dimension::new(Dimension::MIN).unwrap().get(), 1);
        assert_eq!(Dimension::new(Dimension::MAX).unwrap().get(), 65_536);
        assert!(matches!(Dimension::new(0), Err(MnemeError::Invalid(_))));
        assert!(matches!(
            Dimension::new(65_537),
            Err(MnemeError::Invalid(_))
        ));
    }
}
